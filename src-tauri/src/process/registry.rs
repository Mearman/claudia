use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::process::Child;

/// Information about a running agent process
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub run_id: i64,
    pub agent_id: i64,
    pub agent_name: String,
    pub pid: u32,
    pub started_at: DateTime<Utc>,
    pub project_path: String,
    pub task: String,
    pub model: String,
}

/// Information about a running process with handle
#[allow(dead_code)]
pub struct ProcessHandle {
    pub info: ProcessInfo,
    pub child: Arc<Mutex<Option<Child>>>,
    pub live_output: Arc<Mutex<String>>,
}

/// Registry for tracking active agent processes
pub struct ProcessRegistry {
    processes: Arc<Mutex<HashMap<i64, ProcessHandle>>>, // run_id -> ProcessHandle
}

#[allow(dead_code)]
impl ProcessRegistry {
    pub fn new() -> Self {
        Self {
            processes: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a new running process
    #[allow(clippy::too_many_arguments)]
    pub fn register_process(
        &self,
        run_id: i64,
        agent_id: i64,
        agent_name: String,
        pid: u32,
        project_path: String,
        task: String,
        model: String,
        child: Child,
    ) -> Result<(), String> {
        let mut processes = self.processes.lock().map_err(|e| e.to_string())?;

        let process_info = ProcessInfo {
            run_id,
            agent_id,
            agent_name,
            pid,
            started_at: Utc::now(),
            project_path,
            task,
            model,
        };

        let process_handle = ProcessHandle {
            info: process_info,
            child: Arc::new(Mutex::new(Some(child))),
            live_output: Arc::new(Mutex::new(String::new())),
        };

        processes.insert(run_id, process_handle);
        Ok(())
    }

    /// Unregister a process (called when it completes)
    pub fn unregister_process(&self, run_id: i64) -> Result<(), String> {
        let mut processes = self.processes.lock().map_err(|e| e.to_string())?;
        processes.remove(&run_id);
        Ok(())
    }

    /// Get all running processes
    pub fn get_running_processes(&self) -> Result<Vec<ProcessInfo>, String> {
        let processes = self.processes.lock().map_err(|e| e.to_string())?;
        Ok(processes
            .values()
            .map(|handle| handle.info.clone())
            .collect())
    }

    /// Get a specific running process
    pub fn get_process(&self, run_id: i64) -> Result<Option<ProcessInfo>, String> {
        let processes = self.processes.lock().map_err(|e| e.to_string())?;
        Ok(processes.get(&run_id).map(|handle| handle.info.clone()))
    }

    /// Kill a running process
    pub async fn kill_process(&self, run_id: i64) -> Result<bool, String> {
        let child_arc = {
            let processes = self.processes.lock().map_err(|e| e.to_string())?;
            if let Some(handle) = processes.get(&run_id) {
                handle.child.clone()
            } else {
                return Ok(false);
            }
        };

        // Extract the child from the Arc<Mutex<Option<Child>>> to kill it
        let child_opt = {
            let mut child_guard = child_arc.lock().map_err(|e| e.to_string())?;
            child_guard.take() // Take ownership of the child, leaving None
        };

        if let Some(mut child) = child_opt {
            match child.kill().await {
                Ok(_) => Ok(true),
                Err(e) => Err(format!("Failed to kill process: {}", e)),
            }
        } else {
            Ok(false) // Process was already killed or completed
        }
    }

    /// Check if a process is still running by trying to get its status
    pub async fn is_process_running(&self, run_id: i64) -> Result<bool, String> {
        let processes = self.processes.lock().map_err(|e| e.to_string())?;

        if let Some(handle) = processes.get(&run_id) {
            let child_arc = handle.child.clone();
            drop(processes); // Release the lock before async operation

            let mut child_guard = child_arc.lock().map_err(|e| e.to_string())?;
            if let Some(ref mut child) = child_guard.as_mut() {
                match child.try_wait() {
                    Ok(Some(_)) => {
                        // Process has exited
                        *child_guard = None;
                        Ok(false)
                    }
                    Ok(None) => {
                        // Process is still running
                        Ok(true)
                    }
                    Err(_) => {
                        // Error checking status, assume not running
                        *child_guard = None;
                        Ok(false)
                    }
                }
            } else {
                Ok(false) // No child handle
            }
        } else {
            Ok(false) // Process not found in registry
        }
    }

    /// Append to live output for a process
    pub fn append_live_output(&self, run_id: i64, output: &str) -> Result<(), String> {
        let processes = self.processes.lock().map_err(|e| e.to_string())?;
        if let Some(handle) = processes.get(&run_id) {
            let mut live_output = handle.live_output.lock().map_err(|e| e.to_string())?;
            live_output.push_str(output);
            live_output.push('\n');
        }
        Ok(())
    }

    /// Get live output for a process
    pub fn get_live_output(&self, run_id: i64) -> Result<String, String> {
        let processes = self.processes.lock().map_err(|e| e.to_string())?;
        if let Some(handle) = processes.get(&run_id) {
            let live_output = handle.live_output.lock().map_err(|e| e.to_string())?;
            Ok(live_output.clone())
        } else {
            Ok(String::new())
        }
    }

    /// Cleanup finished processes
    pub async fn cleanup_finished_processes(&self) -> Result<Vec<i64>, String> {
        let mut finished_runs = Vec::new();

        // First, get all run IDs
        let run_ids: Vec<i64> = {
            let processes = self.processes.lock().map_err(|e| e.to_string())?;
            processes.keys().cloned().collect()
        };

        // Check each process without holding the main lock
        for run_id in run_ids {
            if !self.is_process_running(run_id).await? {
                finished_runs.push(run_id);
            }
        }

        // Then remove them from the registry
        {
            let mut processes = self.processes.lock().map_err(|e| e.to_string())?;
            for run_id in &finished_runs {
                processes.remove(run_id);
            }
        }

        Ok(finished_runs)
    }
}

impl Default for ProcessRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Global process registry state
pub struct ProcessRegistryState(pub Arc<ProcessRegistry>);

impl Default for ProcessRegistryState {
    fn default() -> Self {
        Self(Arc::new(ProcessRegistry::new()))
    }
}
