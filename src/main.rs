use clap::Parser;
use options::Options;
use tokio::{sync::mpsc, try_join};
use z7::{Operation, Pushment, Z7};

use crate::nvim::Nvim;
mod nvim;
mod options;
mod output_format;
mod z7;

#[tokio::main]
async fn main() {
    let opt = Options::parse();
    log4rs::init_file("config/log4rs.yaml", Default::default()).unwrap();
    let (doc_sender, doc_recv) = mpsc::channel::<Pushment>(1);
    let (oper_sender, oper_recv) = mpsc::channel::<Operation>(1);
    let mut z7 = Z7::new(doc_sender, &opt);
    let _ = try_join!(
        z7.start(oper_recv, oper_sender.clone()),
        Nvim::start(doc_recv, oper_sender)
    );
}
