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
use tokio::{io::WriteHalf, process::Command, sync::mpsc, time::sleep, try_join};

use crate::z7::{Operation, Pushment};

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
    fn analyze(&self, _input: Notify) {
        // info!("analyze: {:?}", input);
    }
}

#[derive(Clone)]
struct NeovimHandler {
    analyzer: Arc<Mutex<BehaviorAnalyzer>>,
    oper_sender: mpsc::Sender<Operation>,
}

impl NeovimHandler {
    pub fn new(
        analyzer: Arc<Mutex<BehaviorAnalyzer>>,
        oper_sender: mpsc::Sender<Operation>,
    ) -> Self {
        Self {
            analyzer,
            oper_sender,
        }
    }
}

#[async_trait]
impl Handler for NeovimHandler {
    // type Writer = Compat<WriteHalf<Connection>>;
    type Writer = Compat<WriteHalf<Connection>>;

    async fn handle_notify(&self, name: String, args: Vec<Value>, _nvim: Neovim<Self::Writer>) {
        match name.as_str() {
            "nvim_buf_lines_event" => {
                let notify = Notify::from(args);
                self.analyzer.lock().unwrap().analyze(notify);
            }
            "nvim_insert_leave_event" => {
                info!("handle_notify: name: {}, args: {:?}", name, args);
                let _ = self
                    .oper_sender
                    .try_send(Operation::Password("test".to_string()));
            }
            // "nvim_execute_event" => {
            //     info!("handle_notify: name: {}, args: {:?}", name, args);
            //     // nvim.quit_no_save().await.expect("quit nvim error");
            //     nvim.quit_no_save().await.expect("quit nvim error");
            // }
            "nvim_vim_leave_event" => {
                let _ = self.oper_sender.send(Operation::Execute).await;
                info!("handle_notify: name: {}, args: {:?}", name, args);
            }
            _ => {
                info!("handle_notify: name: {}, args: {:?}", name, args);
            }
        }
    }
}

pub struct Nvim;

impl Nvim {
    pub async fn start(
        mut doc_recv: mpsc::Receiver<Pushment>,
        oper_sender: mpsc::Sender<Operation>,
    ) -> tokio::io::Result<()> {
        if let Err(e) = Command::new(NVIMPATH)
            .args(["-u", "NONE", "--listen", "/tmp/nvim-socket-001"])
            .stdout(stdout())
            .spawn()
        {
            error!("Failed to start nvim: {}", e);
            return Err(e)?;
        }
        let path = Path::new("/tmp/nvim-socket-001");
        // wait for /tmp/nvim-socket-001 to be created
        while !path.exists() {
            sleep(Duration::from_millis(10)).await;
        }

        // clone oper_sender to NeovimHandler, it will drop when nvim quit, i want keep it alive;
        let oper_sender_ = oper_sender.clone();
        let handler = NeovimHandler::new(Arc::new(Mutex::new(BehaviorAnalyzer)), oper_sender_);
        let (nvim, io_handle) = new_path(path, handler)
            .await
            .expect("connect to nvim failed");

        // register "nvim_insert_leave_event", then subscribe it
        // nvim_insert_leave_event has been triggered, then check password from buf line, then send password to 7z
        nvim.create_autocmd(
            Value::Array(vec!["InsertLeave".into()]),
            vec![(
                "command".into(),
                Value::String(
                    r#"call rpcnotify(0, "nvim_insert_leave_event", [mode(), nvim_win_get_cursor(0)])"#.into(),
                ),
            )],
        )
        .await
        .expect("create autocmd error");
        nvim.subscribe("nvim_insert_leave_event")
            .await
            .expect("subscribe insert leave event failed");

        // register "nvim_vim_leave_event", then subscribe it
        nvim.create_autocmd(
            Value::Array(vec!["VimLeave".into()]),
            vec![(
                "command".into(),
                Value::String(r#"call rpcnotify(0, "nvim_vim_leave_event")"#.into()),
            )],
        )
        .await
        .expect("create autocmd error");
        nvim.subscribe("nvim_vim_leave_event")
            .await
            .expect("subscribe vim leave event failed");

        // attach buf to subscribe "nvim_buf_lines_event"
        let curbuf = nvim.get_current_buf().await.expect("get current buf error");
        curbuf
            .attach(false, vec![])
            .await
            .expect("attach current buf error");

        // register keymap "cc" to nvim, then nvim will notify "nvim_execute_event" to handler
        nvim.set_keymap(
            "n",
            "cc",
            r#":qa!<CR>"#,
            vec![("silent".into(), true.into())],
        )
        .await
        .expect("set keymap error");
        // nvim.subscribe("nvim_execute_event")
        //     .await
        //     .expect("subscribe execute event failed");

        // receive pushment from 7z, then push to nvim
        let wait_push = async move {
            while let Some(pushment) = doc_recv.recv().await {
                match pushment {
                    Pushment::Full(lines) => {
                        info!("recv pushment: {:?}", lines);
                        let line_count = curbuf.line_count().await.expect("get line count error");
                        let _ = curbuf.set_lines(0, line_count, false, lines).await;
                    }
                    Pushment::Line(_line, _content) => {
                        unreachable!()
                    }
                    Pushment::None => {
                        nvim.quit_no_save().await.expect("quit nvim error");
                    }
                }
            }
            info!("pushment recv closed");
            Result::<(), ()>::Ok(())
        };

        let wait_io = async move {
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
            Result::<(), ()>::Err(())
        };

        let _ = try_join!(wait_push, wait_io);
        Ok(())
    }
}
