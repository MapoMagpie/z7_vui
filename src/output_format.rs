use boxed_macro::Boxed;

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

    pub fn layout_list(&mut self) {
        self.lbs = Lines::new_list();
    }

    pub fn layout_extract(&mut self) {
        self.lbs = Lines::new_extract();
    }
}

pub struct Lines {
    inner: Vec<Box<dyn LineBuilder>>,
}

pub const PASSWORD_LINE: usize = 6;
impl Lines {
    pub fn new() -> Self {
        Self { inner: vec![] }
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
            CaptureLB::new_boxed("Date"),
            CaptureLB::new_boxed("-----"),
            FileListLB::boxed(),
            ErrorLB::boxed(),
        ];
        Self { inner }
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
        ];
        Self { inner }
    }

    fn input(&mut self, input: &str) {
        for lb in self.inner.iter_mut() {
            if lb.input(input) {
                break;
            }
        }
    }

    fn lines(&self) -> Vec<String> {
        self.inner.iter().flat_map(|lb| lb.output()).collect()
    }
}

trait LineBuilder: Send + Sync {
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
struct ExtractToLB {
    inner: String,
}

impl LineBuilder for ExtractToLB {
    fn input(&mut self, str: &str) -> bool {
        self.inner.push_str(str);
        false
    }

    fn output(&self) -> Vec<String> {
        vec![self.inner.clone()]
    }
}

#[derive(Default, Boxed)]
struct PasswordLB {
    inner: String,
}

impl LineBuilder for PasswordLB {
    fn input(&mut self, str: &str) -> bool {
        if str.starts_with("Enter password") {
            self.inner = "Enter password: ".to_string();
            true
        } else if str.starts_with("Input password: ") {
            let password = str.trim_start_matches("Input password: ");
            self.inner = format!("Enter password: {}", password);
            true
        } else {
            false
        }
    }
    fn output(&self) -> Vec<String> {
        if self.inner.is_empty() {
            vec![]
        } else {
            vec![self.inner.clone()]
        }
    }
}

#[derive(Default, Boxed)]
struct FileListLB {
    inner: Vec<String>,
}

impl LineBuilder for FileListLB {
    fn input(&mut self, str: &str) -> bool {
        // work for left 7 years :)
        if str.starts_with("202") || (!self.inner.is_empty() && str.starts_with("-----")) {
            self.inner.push(str.to_string());
            true
        } else {
            false
        }
    }

    fn output(&self) -> Vec<String> {
        self.inner.to_vec()
    }
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
