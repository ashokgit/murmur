use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use anyhow::Result;

pub struct Tunnel {
    child: Arc<Mutex<Option<Child>>>,
    url: Arc<Mutex<Option<String>>>,
}

impl Tunnel {
    pub fn start() -> Result<Self> {
        let spawn_res = Command::new("cloudflared")
            .args(["tunnel", "--url", "http://localhost:3731"])
            .stderr(Stdio::piped())
            .stdout(Stdio::null())
            .spawn();

        let mut child = match spawn_res {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let fallbacks = [
                    "/opt/homebrew/bin/cloudflared",
                    "/usr/local/bin/cloudflared",
                    "/opt/homebrew/sbin/cloudflared",
                    "/usr/bin/cloudflared",
                ];
                let mut fallback_child = None;
                for path in fallbacks {
                    if let Ok(c) = Command::new(path)
                        .args(["tunnel", "--url", "http://localhost:3731"])
                        .stderr(Stdio::piped())
                        .stdout(Stdio::null())
                        .spawn()
                    {
                        tracing::info!("Found cloudflared at fallback path: {}", path);
                        fallback_child = Some(c);
                        break;
                    }
                }
                fallback_child.ok_or_else(|| {
                    anyhow::anyhow!(
                        "cloudflared binary not found in PATH or standard macOS paths (/opt/homebrew/bin, /usr/local/bin).\n\
                        Please install cloudflared using: brew install cloudflared"
                    )
                })?
            }
            Err(e) => return Err(e.into()),
        };

        let stderr = child.stderr.take().ok_or_else(|| anyhow::anyhow!("Failed to capture cloudflared stderr"))?;
        let child_mutex = Arc::new(Mutex::new(Some(child)));
        let url_mutex = Arc::new(Mutex::new(None));

        let url_clone = url_mutex.clone();
        thread::spawn(move || {
            let re = regex::Regex::new(r"https://[a-zA-Z0-9-]+\.trycloudflare\.com").unwrap();
            let reader = BufReader::new(stderr);
            for line in reader.lines() {
                if let Ok(l) = line {
                    // Try substring match first
                    let mut found_url = None;
                    if let Some(pos) = l.find("https://") {
                        let sub = &l[pos..];
                        if let Some(end) = sub.find(".trycloudflare.com") {
                            found_url = Some(sub[..end + 18].to_string());
                        }
                    }
                    // Fallback to regex
                    if found_url.is_none() {
                        if let Some(mat) = re.find(&l) {
                            found_url = Some(mat.as_str().to_string());
                        }
                    }

                    if let Some(url) = found_url {
                        let mut url_lock = url_clone.lock().unwrap();
                        *url_lock = Some(url.clone());
                        tracing::info!("Cloudflare Tunnel active: {}", url);
                    }
                }
            }
        });

        Ok(Tunnel {
            child: child_mutex,
            url: url_mutex,
        })
    }

    pub fn is_alive(&self) -> bool {
        let mut lock = self.child.lock().unwrap();
        if let Some(ref mut child) = *lock {
            match child.try_wait() {
                Ok(None) => true, // Still running
                _ => false,       // Either exited or error checking
            }
        } else {
            false
        }
    }

    pub fn get_url(&self) -> Option<String> {
        self.url.lock().unwrap().clone()
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        let mut lock = self.child.lock().unwrap();
        if let Some(mut child) = lock.take() {
            let _ = child.kill();
            let _ = child.wait();
            tracing::info!("Cloudflare Tunnel terminated");
        }
    }
}
