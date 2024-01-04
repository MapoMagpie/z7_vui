use std::{fs, ops::Range};

use boxed_macro::Boxed;
use log::error;

pub struct Document {
    lbs: Lines,
}

impl Document {
    pub fn new() -> Self {
        Self { lbs: Lines::new() }
    }

    pub fn input(&mut self, input: &str) {
        self.lbs.input(input);
    }

    pub fn output(&self) -> Vec<String> {
        let mut lines = self.lbs.lines();
        lines.dedup();
        lines
    }

    #[allow(dead_code)]
    pub fn files(&self) -> Vec<String> {
        self.lbs.file_list_lb.files()
    }

    pub fn layout_list(&mut self) {
        let mut lbs = Lines::new_list();
        std::mem::swap(&mut self.lbs, &mut lbs);
        self.lbs.file_list_lb = lbs.file_list_lb;
    }

    pub fn layout_extract(&mut self) {
        let mut lbs = Lines::new_extract();
        std::mem::swap(&mut self.lbs, &mut lbs);
        self.lbs.file_list_lb = lbs.file_list_lb;
    }
}

pub struct Lines {
    inner: Vec<Box<dyn LineBuilder>>,
    file_list_lb: FileListLB,
}

pub const PASSWORD_LINE: usize = 6;
impl Lines {
    fn new() -> Self {
        Self {
            inner: vec![],
            file_list_lb: FileListLB::default(),
        }
    }
    fn new_list() -> Self {
        let inner = vec![
            TitleLB::boxed(),
            EmptyLB::boxed(),
            CaptureLB::new_boxed("Listing archive:"), // file name
            CaptureLB::new_boxed("file,"),            // file size
            EmptyLB::boxed(),
            PasswordLB::boxed(),
            EmptyLB::boxed(),
            PropertyLB::boxed(),
            EmptyLB::boxed(),
            ErrorLB::boxed(),
            EmptyLB::boxed(),
        ];
        Self {
            inner,
            file_list_lb: FileListLB::default(),
        }
    }

    fn new_extract() -> Self {
        let inner = vec![
            TitleLB::boxed(),
            EmptyLB::boxed(),
            CaptureLB::new_boxed("Extracting archive:"), // file name
            CaptureLB::new_boxed("file,"),               // file size
            EmptyLB::boxed(),
            PasswordLB::boxed(),
            EmptyLB::boxed(),
            PropertyLB::boxed(),
            EmptyLB::boxed(),
            CaptureLB::new_boxed("Everything"), // file name
            ErrorLB::boxed(),
            EmptyLB::boxed(),
        ];
        Self {
            inner,
            file_list_lb: FileListLB::default(),
        }
    }

    fn input(&mut self, input: &str) {
        if self.file_list_lb.input(input) {
            return;
        }
        for lb in self.inner.iter_mut() {
            if lb.input(input) {
                break;
            }
        }
    }

    fn lines(&self) -> Vec<String> {
        let mut lines: Vec<String> = self.inner.iter().flat_map(|lb| lb.output()).collect();
        lines.append(&mut self.file_list_lb.output());
        lines
    }
}

trait LineBuilder: Send + Sync + 'static {
    /// return true if LineBuilder take this input,
    /// do not pass it to other LineBuilder
    fn input(&mut self, _: &str) -> bool {
        false
    }
    fn output(&self) -> Vec<String>;
}

trait BoxedDefault {
    fn boxed() -> Box<dyn LineBuilder>;
}

#[derive(Boxed)]
struct TitleLB {
    inner: String,
}

impl Default for TitleLB {
    fn default() -> Self {
        let title = r#"7Z-VUI, Shortcuts: `space+c`: execute extract|add; `space+q`: Quit this program; `space+r`: Retry"#;
        Self {
            inner: title.to_string(),
        }
    }
}

impl LineBuilder for TitleLB {
    fn output(&self) -> Vec<String> {
        vec![self.inner.clone()]
    }
}

#[derive(Default, Boxed)]
struct EmptyLB;

impl LineBuilder for EmptyLB {
    fn output(&self) -> Vec<String> {
        vec!["".to_string()]
    }
}

#[derive(Default, Boxed)]
struct PasswordLB {
    inner: Vec<String>,
    password_history: Vec<String>,
}

impl LineBuilder for PasswordLB {
    fn input(&mut self, str: &str) -> bool {
        // init password history
        if (str.starts_with("Enter password") || str.starts_with("Input passsword"))
            && self.inner.is_empty()
        {
            self.inner.push(String::new());
        }
        if str.starts_with("Password history file: ") {
            // read password history from file config/password_history.txt
            if let Ok(password_history) =
                fs::read_to_string(str.trim_start_matches("Password history file: "))
            {
                self.password_history = password_history
                    .lines()
                    .map(|line| line.trim().to_string())
                    .filter(|line| !line.is_empty())
                    .collect::<Vec<String>>();
                if self.inner.len() > 1 {
                    self.inner.pop();
                }
                self.inner.push(format!(
                    "select password use [Ctrl+x]: {}",
                    self.password_history.join(" | ")
                ));
            }
            return true;
        }
        if str.starts_with("Enter password") {
            self.inner[0] = "Enter password: ".to_string();
            true
        } else if str.starts_with("Input password") {
            let password = str.trim_start_matches("Input password: ");
            self.inner[0] = format!("Enter password: {}", password);
            true
        } else if str.starts_with("Save password") && self.inner.len() >= 2 {
            self.password_history
                .push(str.trim_start_matches("Save password: ").to_string());
            self.password_history.sort();
            self.password_history.dedup();
            fs::write(
                "config/password_history.txt",
                self.password_history.join("\n"),
            )
            .expect("write password history failed");
            true
        } else {
            false
        }
    }
    fn output(&self) -> Vec<String> {
        self.inner.to_vec()
    }
}

struct FileLine {
    filename: String,
    raw: String,
}

impl FileLine {
    fn to_string(&self, extract_path: &str) -> String {
        format!("{}{}{}", self.raw, extract_path, self.filename)
    }
}

impl From<(&str, &[Range<usize>; 5])> for FileLine {
    fn from((str, tem): (&str, &[Range<usize>; 5])) -> Self {
        let chars = str.chars().collect::<Vec<char>>();
        if chars.len() < tem[0].start || chars.len() < tem[4].start {
            error!("parse file line failed: {}", str);
        }
        let prefix = String::from_iter(&chars[tem[0].start..tem[4].start]);
        let filename = String::from_iter(&chars[tem[4].start..]);
        Self {
            filename,
            raw: prefix,
        }
    }
}

#[derive(Default, Boxed)]
struct FileListLB {
    inner: Vec<FileLine>,
    header_line: Option<String>,
    begin_line: Option<String>,
    end_line: Option<String>,
    template: Option<[Range<usize>; 5]>,
    summary_line: String,
    capture: bool,
    extract_path: String,
}

impl FileListLB {
    fn files(&self) -> Vec<String> {
        self.inner.iter().map(|f| f.filename.clone()).collect()
    }
}

impl LineBuilder for FileListLB {
    fn input(&mut self, str: &str) -> bool {
        if str.starts_with("-----") {
            if self.begin_line.is_none() {
                self.template = Some(parse_dash_line_to_range(str));
                self.begin_line = Some(str.to_string());
                self.capture = true;
            } else {
                self.end_line = Some(str.to_string());
            }
            true
        } else if self.capture {
            // capture the summary line
            if self.end_line.is_some() {
                self.capture = false;
                self.summary_line = str.to_string();
            } else if str.is_empty() {
                error!("occurs empty line in file list");
            } else {
                self.inner
                    .push(FileLine::from((str, self.template.as_ref().unwrap())));
            }
            true
        } else if str.contains("Attr") {
            self.header_line = Some(str.to_string());
            true
        } else if str.starts_with("Set extract_path:") {
            self.extract_path = str
                .trim_start_matches("Set extract_path:")
                .trim()
                .to_string();
            true
        } else {
            false
        }
    }

    fn output(&self) -> Vec<String> {
        let files = self
            .inner
            .iter()
            .map(|f| f.to_string(&self.extract_path))
            .collect();
        [
            self.header_line.clone().map_or(vec![], |l| vec![l]),
            self.begin_line.clone().map_or(vec![], |l| vec![l]),
            files,
            self.end_line
                .clone()
                .map_or(vec![], |l| vec![l, self.summary_line.clone()]),
        ]
        .concat()
    }
}

fn parse_dash_line_to_range(line: &str) -> [Range<usize>; 5] {
    let mut ra: [Range<usize>; 5] = Default::default();
    let mut cur_i = 0;
    let mut start = 0;
    let mut len = 0;
    let mut last_c = ' ';
    for (i, c) in line.char_indices() {
        len += 1;
        if c == ' ' {
            if last_c == ' ' {
                start = i + 1;
                continue;
            }
            ra[cur_i].start = start;
            ra[cur_i].end = i;
            cur_i += 1;
            start = i + 1;
        }
        last_c = c;
    }
    ra[cur_i].start = start;
    ra[cur_i].end = len;
    ra
}

#[derive(Default, Boxed)]
struct PropertyLB {
    inner: String,
    done: bool,
}

impl LineBuilder for PropertyLB {
    fn input(&mut self, input: &str) -> bool {
        if self.done {
            false
        } else if input.starts_with("Type = ") {
            self.inner.push_str(input);
            true
        } else if input.starts_with("Method = ") {
            self.inner.push('\t');
            self.inner.push_str(input);
            self.done = true;
            true
        } else {
            false
        }
    }

    fn output(&self) -> Vec<String> {
        vec![self.inner.clone()]
    }
}

struct CaptureLB {
    inner: String,
    done: bool,
    expression: String,
}

impl CaptureLB {
    fn new(expression: &str) -> Self {
        Self {
            inner: String::new(),
            done: false,
            expression: expression.to_string(),
        }
    }
    fn new_boxed(expression: &str) -> Box<dyn LineBuilder> {
        Box::new(Self::new(expression))
    }
}

impl LineBuilder for CaptureLB {
    fn input(&mut self, input: &str) -> bool {
        if self.done {
            false
        } else if input.contains(&self.expression) {
            self.inner.push_str(input);
            self.done = true;
            true
        } else {
            false
        }
    }
    fn output(&self) -> Vec<String> {
        vec![self.inner.clone()]
    }
}

#[derive(Default, Boxed)]
struct ErrorLB {
    inner: String,
}

impl LineBuilder for ErrorLB {
    fn input(&mut self, input: &str) -> bool {
        if input.starts_with("ERROR:") {
            self.inner.push_str(input);
            true
        } else {
            false
        }
    }
    fn output(&self) -> Vec<String> {
        vec![self.inner.clone()]
    }
}

#[cfg(test)]
mod test {

    use std::env;

    use super::{parse_dash_line_to_range, FileListLB, LineBuilder};
    #[test]
    fn test_parse_dash_line_to_range() {
        let ra = parse_dash_line_to_range("--- --- ---- ---- -----");
        assert_eq!(ra, [0..3, 4..7, 8..12, 13..17, 18..23]);
        let ra = parse_dash_line_to_range("--- --- ---- ----  -----");
        assert_eq!(ra, [0..3, 4..7, 8..12, 13..17, 19..24]);
    }

    #[test]
    fn test_file_list_lb() {
        let mut flb = FileListLB::default();
        let raw = r##"
------------------- ----- ------------ ------------  ------------------------
2023-12-22 16:17:58 D....            0            0  test
2023-12-12 09:18:24 ....A       344963     13216256  test/01-e_01.png
2023-12-12 09:18:28 ....A       821434               test/02-e_02.png
2023-12-12 09:18:26 ....A       608418               test/03-e_03.png
2023-12-12 09:18:28 ....A       757826               test/04-e_04.png
2023-12-12 09:18:28 ....A       790792               test/05-e_05.png
2023-12-12 09:18:30 ....A       712878               test/06-e_06.png
2023-12-12 09:18:30 ....A       740854               test/07-e_07.png
2023-12-12 09:18:30 ....A       711147               test/08-e_08.png
2023-12-12 09:18:32 ....A       724006               test/09-e_09.png
2023-12-12 09:18:32 ....A       637246               test/10-e_10.png
2023-12-12 09:18:32 ....A       739784               test/11-e_11.png
2023-12-12 09:18:34 ....A       733386               test/12-e_12.png
2023-12-12 09:18:34 ....A       683368               test/13-e_13.png
2023-12-12 09:18:34 ....A       740540               test/14-e_14.png
2023-12-12 09:18:38 ....A       781016               test/15-e_15.png
2023-12-12 09:18:38 ....A       681962               test/16-e_16.png
2023-12-12 09:18:38 ....A       830510               test/17-e_17.png
2023-12-12 09:18:40 ....A       220106               test/18-e_18.png
2023-12-12 09:18:40 ....A       436832               test/19-e_19.png
2023-12-12 09:18:40 ....A       311853               test/20-e_20.png
2023-12-12 09:18:42 ....A       328685               test/21-MT43.jpg
2023-12-12 09:18:42 ....A          473               test/meta.json
------------------- ----- ------------ ------------  ------------------------
2023-12-22 16:17:58           13338079     13216256  22 files, 1 folders
"##;
        raw.lines().for_each(|l| {
            let _ = flb.input(l);
        });
        let lb: Box<dyn LineBuilder> = Box::new(flb);
        lb.output().iter().for_each(|l| {
            println!("{}", l);
        });

        let a = lb.as_ref() as *const dyn LineBuilder as *const FileListLB;
        let a = unsafe { &*a };
        a.files().iter().for_each(|f| {
            println!("{}", f);
        });
    }

    #[test]
    fn test_path() {
        let mut path = env::current_dir().expect("cwd failed");
        path.push("test.7z");
        dbg!(&path);
        dbg!(path.is_absolute());
        dbg!(path.is_relative());
        let mut path = env::current_dir().expect("cwd failed");
        path.push("/home/kamo-death/code/vui-7z/stderr.txt");
        dbg!(&path);
        dbg!(path.is_absolute());
        dbg!(path.is_relative());
        let mut path = env::current_dir().expect("cwd failed");
        path.push("$HOME/code/vui-7z/stderr.txt");
        dbg!(&path);
        dbg!(path.is_absolute());
        dbg!(path.is_relative());
        // let str = fs::read_to_string(path).unwrap();
        // dbg!(str);
    }
}
