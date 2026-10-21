use super::*;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    io::{Read, Write},
    sync::{Arc, Mutex},
};

struct Terminal {
    owner: String,
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    output: Mutex<VecDeque<u8>>,
    cancel: CancellationToken,
    _guards: Mutex<Vec<Box<dyn std::any::Any + Send>>>,
}
#[derive(Default, Clone)]
pub struct PtyManager {
    terminals: Arc<Mutex<HashMap<String, Arc<Terminal>>>>,
}
impl PtyManager {
    pub fn start(
        &self,
        native: &NativeSandbox,
        request: ExecRequest,
        owner: String,
        cancel: CancellationToken,
    ) -> Result<Value> {
        let mut terminals = self
            .terminals
            .lock()
            .map_err(|_| anyhow::anyhow!("terminal lock poisoned"))?;
        ensure!(terminals.len() < 32, "terminal quota exhausted");
        let launch = native.confined_command(&request)?;
        let std_command = launch.command.as_std();
        let mut builder = CommandBuilder::new(std_command.get_program());
        builder.env_clear();
        for arg in std_command.get_args() {
            if arg != "--new-session" {
                builder.arg(arg);
            }
        }
        for (k, v) in std_command.get_envs() {
            if let Some(v) = v {
                builder.env(k, v);
            }
        }
        builder.env("TERM", "xterm-256color");
        let pair = portable_pty::native_pty_system().openpty(PtySize {
            rows: 24,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let child = pair.slave.spawn_command(builder)?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let id = layeragent_protocol::new_id();
        let lifecycle = cancel.child_token();
        let terminal = Arc::new(Terminal {
            owner,
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            child: Mutex::new(child),
            output: Mutex::new(VecDeque::new()),
            cancel: lifecycle.clone(),
            _guards: Mutex::new(launch.guards),
        });
        let read_state = terminal.clone();
        let maximum = request.policy.max_output_bytes;
        std::thread::spawn(move || {
            let mut total = 0;
            let mut buffer = [0; 4096];
            while let Ok(n) = reader.read(&mut buffer) {
                if n == 0 {
                    break;
                }
                total += n;
                if let Ok(mut output) = read_state.output.lock() {
                    output.extend(&buffer[..n]);
                    if output.len() > maximum {
                        output.truncate(maximum);
                    }
                }
                if total > maximum {
                    read_state.cancel.cancel();
                    break;
                }
            }
        });
        let watch_state = terminal.clone();
        let timeout = Duration::from_secs(request.policy.timeout_secs);
        std::thread::spawn(move || {
            let start = std::time::Instant::now();
            loop {
                if let Ok(mut child) = watch_state.child.lock()
                    && child.try_wait().ok().flatten().is_some()
                {
                    break;
                }
                if lifecycle.is_cancelled() || start.elapsed() >= timeout {
                    if let Ok(mut writer) = watch_state.writer.try_lock() {
                        let _ = writer.write_all(b"\x03");
                        let _ = writer.flush();
                    }
                    std::thread::sleep(Duration::from_millis(1500));
                    if let Ok(mut child) = watch_state.child.lock()
                        && child.try_wait().ok().flatten().is_none()
                    {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        });
        terminals.insert(id.clone(), terminal);
        Ok(json!({"pty_id":id,"rows":24,"cols":100}))
    }
    fn terminal(&self, id: &str, owner: &str) -> Result<Arc<Terminal>> {
        let terminals = self
            .terminals
            .lock()
            .map_err(|_| anyhow::anyhow!("terminal lock poisoned"))?;
        let terminal = terminals.get(id).context("terminal not found")?;
        ensure!(terminal.owner == owner, "terminal inaccessible");
        Ok(terminal.clone())
    }
    pub fn read(&self, id: &str, owner: &str) -> Result<Value> {
        let t = self.terminal(id, owner)?;
        let bytes = t
            .output
            .lock()
            .map_err(|_| anyhow::anyhow!("output lock poisoned"))?
            .drain(..)
            .collect::<Vec<_>>();
        let status = t
            .child
            .lock()
            .map_err(|_| anyhow::anyhow!("child lock poisoned"))?
            .try_wait()?;
        Ok(
            json!({"text":String::from_utf8_lossy(&bytes),"bytes_hex":hex::encode(&bytes),"exited":status.is_some(),"exit_code":status.map(|s|s.exit_code())}),
        )
    }
    pub fn write(&self, id: &str, owner: &str, text: &str) -> Result<Value> {
        ensure!(text.len() <= 16_384, "terminal input exceeds limit");
        let t = self.terminal(id, owner)?;
        let mut writer = t
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("writer lock poisoned"))?;
        writer.write_all(text.as_bytes())?;
        writer.flush()?;
        Ok(json!({"written":text.len()}))
    }
    pub fn resize(&self, id: &str, owner: &str, rows: u16, cols: u16) -> Result<Value> {
        ensure!(
            (1..=500).contains(&rows) && (1..=500).contains(&cols),
            "invalid terminal dimensions"
        );
        let t = self.terminal(id, owner)?;
        t.master
            .lock()
            .map_err(|_| anyhow::anyhow!("master lock poisoned"))?
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })?;
        Ok(json!({"resized":true}))
    }
    pub fn kill(&self, id: &str, owner: &str) -> Result<Value> {
        let t = self.terminal(id, owner)?;
        t.cancel.cancel();
        Ok(json!({"cancelled":true}))
    }
    pub fn close_owner(&self, owner: &str) {
        if let Ok(mut terminals) = self.terminals.lock() {
            terminals.retain(|_, terminal| {
                if terminal.owner == owner {
                    terminal.cancel.cancel();
                    false
                } else {
                    true
                }
            });
        }
    }
}
