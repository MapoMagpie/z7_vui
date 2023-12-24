use std::{
    fmt::Debug,
    io::stdout,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use log::{error, info};
use nvim_rs::{compat::tokio::Compat, create::tokio::new_path, Handler, Neovim, Value};
use parity_tokio_ipc::Connection;
use tokio::{io::WriteHalf, process::Command, time::sleep};

// const OUTPUT_FILE: &str = "handler_drop.txt";
const NVIMPATH: &str = "nvim";

pub struct Notify {
    line_start: u64,
    line_end: u64,
    buf_id: i64,
    content: Vec<String>,
}

impl From<Vec<Value>> for Notify {
    fn from(args: Vec<Value>) -> Self {
        let line_start = args[2].as_u64().unwrap();
        let line_end = args[3].as_u64().unwrap();
        let buf_id = args[1].as_i64().unwrap();
        let content = args[4].as_array().unwrap();
        let mut content_vec = Vec::new();
        content.iter().for_each(|v| {
            content_vec.push(v.as_str().unwrap().to_string());
        });
        Self {
            line_start,
            line_end,
            buf_id,
            content: content_vec,
        }
    }
}

impl Debug for Notify {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "(buf_id: {}, line_start: {}, line_end: {}, content: {:?})",
            self.buf_id, self.line_start, self.line_end, self.content
        )
    }
}

pub struct BehaviorAnalyzer;

impl BehaviorAnalyzer {
    fn analyze(&self, input: Notify) {
        info!("analyze: {:?}", input);
    }
}

#[derive(Clone)]
struct NeovimHandler {
    analyzer: Arc<Mutex<BehaviorAnalyzer>>,
}

impl NeovimHandler {
    pub fn new(analyzer: Arc<Mutex<BehaviorAnalyzer>>) -> Self {
        Self { analyzer }
    }
}

#[async_trait]
impl Handler for NeovimHandler {
    // type Writer = Compat<WriteHalf<Connection>>;
    type Writer = Compat<WriteHalf<Connection>>;

    async fn handle_notify(&self, name: String, args: Vec<Value>, _neovim: Neovim<Self::Writer>) {
        if name == "nvim_buf_lines_event" {
            let notify = Notify::from(args);
            self.analyzer.lock().unwrap().analyze(notify);
        } else {
            info!("handle_notify: name: {}, args: {:?}", name, args);
        }
    }
}

pub struct Nvim;

impl Nvim {
    pub async fn start() -> tokio::io::Result<()> {
        let handler = NeovimHandler::new(Arc::new(Mutex::new(BehaviorAnalyzer)));

        if let Err(e) = Command::new(NVIMPATH)
            .args(["-u", "NONE", "--listen", "/tmp/nvim-socket-001"])
            // .env("NVIM_LOG_FILE", "nvimlog")
            .stdout(stdout())
            .spawn()
        {
            error!("Failed to start nvim: {}", e);
            return Err(e)?;
        }

        info!("init nvim ok, wait socket created");
        let path = Path::new("/tmp/nvim-socket-001");
        // wait for /tmp/nvim-socket-001 to be created
        while !path.exists() {
            sleep(Duration::from_millis(10)).await;
        }

        info!("socket path created, waiting for connection");
        let (nvim, io_handle) = new_path(path, handler)
            .await
            .expect("connect to nvim failed");
        info!("connected to nvim");
        // info!("init nvim ok, wait ui attach");
        // nvim.ui_attach(200, 200, UiAttachOptions::new().set_override(false))
        //     .await
        //     .expect("attach ui error");
        // info!("ui attach ok");
        // let uis = nvim.list_uis().await.expect("list uis error");
        // info!("list uis: {:?}", uis);
        let curbuf = nvim.get_current_buf().await.expect("get current buf error");
        if !curbuf
            .attach(false, vec![])
            .await
            .expect("attach current buf error")
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "attach error",
            ))?;
        }
        match io_handle.await {
            Ok(Ok(())) => {
                info!("everything ok!");
            }
            Ok(Err(e)) => {
                error!("loop error: {:?}", e);
            }
            Err(e) => {
                error!("join error: {:?}", e);
            }
        }
        Ok(())
    }
}
