use boxed_macro::Boxed;

const PASSWORD_LINE: usize = 4;

pub struct Document {
    // inner: Rope,
    lbs: Lines,
}

impl Document {
    pub fn new() -> Self {
        Self {
            // inner: Rope::from_str(title),
            lbs: Lines::new(),
        }
    }

    pub fn input(&mut self, input: &str) {
        self.lbs.input(input);
    }

    pub fn output(&self) -> Vec<String> {
        self.lbs.lines()
    }

    pub fn clear(&mut self) {
        self.lbs = Lines::new();
    }
}

struct Lines {
    inner: Vec<Box<dyn LineBuilder>>,
}

impl Lines {
    fn new() -> Self {
        Self {
            // inner: vec![FileSizeLB::boxed(), FilenameLB::boxed()],
            inner: vec![],
        }
    }
    fn input(&mut self, input: &str) {
        let mut line = CommonLB::boxed();
        line.input(input);
        self.inner.push(line);
        // self.inner.iter_mut().for_each(|lb| lb.input(input));
    }

    fn lines(&self) -> Vec<String> {
        self.inner.iter().map(|lb| lb.output()).collect()
    }
}

trait LineBuilder {
    fn input(&mut self, input: &str);
    fn output(&self) -> String;
}

trait BoxedDefault {
    fn boxed() -> Box<dyn LineBuilder>;
}

#[derive(Default, Boxed)]
struct FileSizeLB {
    inner: String,
}

impl LineBuilder for FileSizeLB {
    fn input(&mut self, _: &str) {
        todo!()
    }

    fn output(&self) -> String {
        self.inner.clone()
    }
}

#[derive(Default, Boxed)]
struct FilenameLB {
    inner: String,
}

impl LineBuilder for FilenameLB {
    fn input(&mut self, str: &str) {
        if str.starts_with("Listing archive: ") {
            self.inner.push_str(str);
        }
    }

    fn output(&self) -> String {
        self.inner.clone()
    }
}

#[derive(Boxed)]
struct TitleLB {
    inner: String,
}

impl Default for TitleLB {
    fn default() -> Self {
        let title = r#"7Z-VUI, Shortcuts: `cc`: execute extract|add; `Ctrl+c`: Quit this program;"#;
        Self {
            inner: title.to_string(),
        }
    }
}

impl LineBuilder for TitleLB {
    fn input(&mut self, _: &str) {}
    fn output(&self) -> String {
        self.inner.clone()
    }
}

#[derive(Default, Boxed)]
struct EmptyLB;

impl LineBuilder for EmptyLB {
    fn input(&mut self, _: &str) {}
    fn output(&self) -> String {
        "".to_string()
    }
}

#[derive(Default, Boxed)]
struct CommonLB {
    inner: String,
}

impl LineBuilder for CommonLB {
    fn input(&mut self, str: &str) {
        self.inner.push_str(str);
    }

    fn output(&self) -> String {
        self.inner.clone()
    }
}
