use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Options {
    /// Input file that is a archive file, It's Required;
    pub file: FilePath,
    /// password history file
    #[arg(short = 'p', long = "password-history", default_value_t = default_password_history_file())]
    pub password_history_file: String,
}

#[derive(Clone, Debug)]
pub struct FilePath {
    pub file: String,
}

impl From<String> for FilePath {
    fn from(file: String) -> Self {
        let mut current_dir = std::env::current_dir().unwrap();
        current_dir.push(file);
        Self {
            file: current_dir.to_str().unwrap().to_string(),
        }
    }
}

fn default_password_history_file() -> String {
    let path = PathBuf::from(env!("HOME"))
        .join(".config")
        .join("7zvui")
        .join("password_history.txt");
    // let path = PathBuf::from(env!("HOME")).join("code/vui-7z/config/password_history.txt");
    path.to_str().unwrap().to_string()
}
