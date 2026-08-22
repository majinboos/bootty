use bootty_config::{
    color::Color,
    config::{ConfigDocument, ConfigResult},
};

macro_rules! draft_setter {
    ($method:ident($value:ident: $type:ty)) => {
        pub(super) fn $method(&mut self, path: &[&str], $value: $type) {
            self.mutate(|document| document.$method(path, $value));
        }
    };
}

pub(super) struct SettingsWriteback {
    document: ConfigDocument,
    dirty: bool,
    submit: bool,
    last_error: Option<String>,
}

impl SettingsWriteback {
    pub(super) fn new(document: ConfigDocument) -> Self {
        Self {
            document,
            dirty: false,
            submit: false,
            last_error: None,
        }
    }

    pub(super) fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub(super) fn set_error(&mut self, error: impl ToString) {
        self.last_error = Some(error.to_string());
    }

    pub(super) fn take_submission(&mut self) -> Option<ConfigDocument> {
        std::mem::take(&mut self.submit).then(|| self.document.clone())
    }

    pub(super) fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub(super) fn accept(&mut self, document: ConfigDocument, warning: Option<String>) {
        self.document = document;
        self.dirty = false;
        self.submit = false;
        self.last_error = warning;
    }

    pub(super) fn sync_accepted(&mut self, document: ConfigDocument) {
        if !self.dirty {
            self.document = document;
        }
    }

    pub(super) fn mutate(
        &mut self,
        mutation: impl FnOnce(&mut ConfigDocument) -> ConfigResult<()>,
    ) {
        match mutation(&mut self.document) {
            Ok(()) => {
                self.dirty = true;
                self.submit = true;
            }
            Err(error) => self.set_error(error),
        }
    }

    draft_setter!(set_f32(value: f32));
    draft_setter!(set_bool(value: bool));
    draft_setter!(set_str(value: &str));
    draft_setter!(set_i64(value: i64));
    draft_setter!(set_env(value: &[(String, String)]));

    pub(super) fn set_color(&mut self, path: &[&str], rgb: [u8; 3]) {
        self.set_color_value(
            path,
            Color {
                r: rgb[0],
                g: rgb[1],
                b: rgb[2],
                a: 0xff,
            },
        );
    }

    pub(super) fn set_color_value(&mut self, path: &[&str], color: Color) {
        let hex = color_hex(color);
        self.mutate(move |document| document.set_str(path, &hex));
    }

    draft_setter!(set_strings(value: &[String]));

    pub(super) fn contains(&self, path: &[&str]) -> bool {
        self.document.contains(path)
    }

    pub(super) fn string_array(&self, path: &[&str]) -> Option<Vec<String>> {
        self.document.string_array(path)
    }

    pub(super) fn remove(&mut self, path: &[&str]) {
        self.mutate(|document| document.remove(path));
    }
}

fn color_hex(color: Color) -> String {
    format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b)
}
