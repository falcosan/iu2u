use std::ptr::NonNull;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use objc2_application_services::{
    AXCopyMultipleAttributeOptions, AXError, AXIsProcessTrusted, AXUIElement, AXValue, AXValueType,
};
use objc2_core_foundation::{
    CFArray, CFBoolean, CFIndex, CFNumber, CFRange, CFRetained, CFString, CFType, CGSize,
};
use objc2_core_graphics::{CGEvent, CGEventFlags};

use super::screen::{Element, Kind, Node, Screen};
use super::surface::{self, Action, FIRST_CHANGE, POLL, Probe};

const ATTRIBUTES: [&str; 12] = [
    "AXRole",
    "AXSubrole",
    "AXTitle",
    "AXDescription",
    "AXValue",
    "AXEnabled",
    "AXFocused",
    "AXSelected",
    "AXExpanded",
    "AXPlaceholderValue",
    "AXSize",
    "AXChildren",
];
const MAX_NODES: usize = 3000;
const WALK_WITHIN: Duration = Duration::from_secs(2);
const LAUNCH_WITHIN: Duration = Duration::from_secs(10);
const MESSAGING_TIMEOUT: f32 = 0.25;
const RETURN: u16 = 36;
const PAGE_DOWN: u16 = 121;
const LETTER_A: u16 = 0;

pub(super) struct Mac {
    pid: i32,
    app: CFRetained<AXUIElement>,
    attributes: CFRetained<CFArray<CFString>>,
    handles: Vec<CFRetained<AXUIElement>>,
    fingerprint: String,
    was_manual: bool,
    limit: usize,
}

#[derive(Default)]
struct Read {
    role: String,
    subrole: String,
    title: String,
    description: String,
    text: String,
    number: Option<i64>,
    enabled: Option<bool>,
    focused: Option<bool>,
    selected: Option<bool>,
    expanded: Option<bool>,
    placeholder: String,
    empty: bool,
    children: Vec<CFRetained<AXUIElement>>,
}

impl Mac {
    pub(super) async fn launch(name: &str) -> Result<Self> {
        anyhow::ensure!(
            unsafe { AXIsProcessTrusted() },
            "allow the app that runs iu2u, such as your terminal or editor, in System Settings > Privacy & Security > Accessibility"
        );
        let opened = tokio::process::Command::new("open")
            .args(["-g", "-a", name])
            .status()
            .await?;
        anyhow::ensure!(opened.success(), "no app named {name:?}");
        let folder = executables_of(name)?;
        let manual = CFString::from_str("AXManualAccessibility");
        let started = Instant::now();
        let (mut asked, mut running_here) = (Vec::<(i32, bool)>::new(), false);
        let shown = loop {
            let shown = running()
                .into_iter()
                .rev()
                .filter(|&pid| executable(pid).to_lowercase().contains(&folder))
                .find_map(|pid| {
                    running_here = true;
                    let app = unsafe { AXUIElement::new_application(pid) };
                    unsafe { app.set_messaging_timeout(MESSAGING_TIMEOUT) };
                    let mut windows = 0;
                    let counted = unsafe {
                        app.attribute_value_count(
                            &CFString::from_str("AXWindows"),
                            NonNull::from(&mut windows),
                        )
                    };
                    if counted != AXError::Success {
                        return None;
                    }
                    if asked.iter().all(|&(known, _)| known != pid) {
                        asked.push((pid, switched_on(&app, &manual)));
                        unsafe { app.set_attribute_value(&manual, CFBoolean::new(true)) };
                    }
                    (windows > 0).then_some((pid, app))
                });
            if shown.is_some() || started.elapsed() >= LAUNCH_WITHIN {
                break shown;
            }
            tokio::time::sleep(POLL).await;
        };
        let driven = shown.as_ref().map(|(pid, _)| *pid);
        for &(pid, was_manual) in &asked {
            if !was_manual && driven != Some(pid) {
                unsafe {
                    AXUIElement::new_application(pid)
                        .set_attribute_value(&manual, CFBoolean::new(false));
                }
            }
        }
        let Some((pid, app)) = shown else {
            if running_here {
                bail!("{name} showed no window it would describe");
            }
            bail!("{name} did not start");
        };
        Ok(Self {
            pid,
            app,
            attributes: CFArray::from_retained_objects(&ATTRIBUTES.map(CFString::from_str)),
            handles: Vec::new(),
            fingerprint: String::new(),
            was_manual: asked.contains(&(pid, true)),
            limit: MAX_NODES,
        })
    }

    pub(super) fn observe(&mut self) -> Result<Vec<Node>> {
        let started = Instant::now();
        let mut nodes: Vec<Node> = Vec::new();
        let mut handles = Vec::new();
        let mut stack: Vec<(CFRetained<AXUIElement>, Option<usize>)> =
            vec![(self.app.clone(), None)];
        while let Some((element, parent)) = stack.pop() {
            if nodes.len() >= self.limit || started.elapsed() >= WALK_WITHIN {
                self.limit = next_limit(self.limit, nodes.len(), started.elapsed());
                cut_short(&mut nodes);
                break;
            }
            let Some(read) = self.read(&element) else {
                continue;
            };
            if read.empty || read.role == "AXMenuBar" {
                continue;
            }
            let index = nodes.len();
            nodes.push(read.node(handles.len()));
            handles.push(element);
            if let Some(parent) = parent {
                nodes[parent].children.push(index);
            }
            stack.extend(
                read.children
                    .into_iter()
                    .rev()
                    .map(|child| (child, Some(index))),
            );
        }
        anyhow::ensure!(!nodes.is_empty(), "the app did not answer");
        self.handles = handles;
        self.fingerprint = Screen::new(&nodes, &[]).fingerprint();
        Ok(nodes)
    }

    pub(super) async fn act(&mut self, action: &Action) -> Result<()> {
        self.limit = MAX_NODES;
        let before = self.fingerprint.clone();
        let started = Instant::now();
        match action {
            Action::Press(element) => {
                let target = self.fresh(element)?;
                if unsafe { target.perform_action(&CFString::from_str("AXPress")) }
                    != AXError::Success
                    && !self.set(&target, "AXSelected", CFBoolean::new(true))
                {
                    bail!("{} cannot be pressed", element.describe());
                }
            }
            Action::Type(element, input) => {
                let target = self.fresh(element)?;
                let before = self.read(&target).map(|read| read.text).unwrap_or_default();
                let taken = |read: &Read| read.text == input.text || read.text != before;
                self.set(&target, "AXFocused", CFBoolean::new(true));
                let set = self.set(&target, "AXValue", &CFString::from_str(&input.text));
                let read = self.read(&target);
                if !(set && read.as_ref().is_some_and(taken)) {
                    anyhow::ensure!(
                        read.as_ref().is_some_and(|read| read.focused == Some(true)),
                        "{} cannot take focus",
                        element.describe()
                    );
                    let secure = read
                        .as_ref()
                        .is_some_and(|read| read.subrole == "AXSecureTextField");
                    let length = read
                        .as_ref()
                        .map_or(0, |read| read.text.encode_utf16().count());
                    let all = CFRange {
                        location: 0,
                        length: length as CFIndex,
                    };
                    if let Some(range) =
                        unsafe { AXValue::new(AXValueType::CFRange, NonNull::from(&all).cast()) }
                    {
                        self.set(&target, "AXSelectedTextRange", &range);
                    }
                    self.key(LETTER_A, CGEventFlags::MaskCommand, None);
                    for character in input.text.chars() {
                        self.key(0, CGEventFlags::empty(), Some(character));
                    }
                    let typed_at = Instant::now();
                    let mut took = secure;
                    while !took && typed_at.elapsed() < FIRST_CHANGE {
                        tokio::time::sleep(POLL).await;
                        took = self.read(&target).as_ref().is_some_and(taken);
                    }
                    anyhow::ensure!(took, "{} did not take the text", element.describe());
                }
            }
            Action::Choose(element, _) => {
                bail!("{} has no options to choose from", element.describe())
            }
            Action::Enter => self.key(RETURN, CGEventFlags::empty(), None),
            Action::Scroll => self.key(PAGE_DOWN, CGEventFlags::empty(), None),
            Action::Wait => {}
        }
        surface::settle(Some(before), started, async || {
            self.observe().ok();
            Probe {
                revision: Some(self.fingerprint.clone()),
                busy: false,
                loading: false,
            }
        })
        .await;
        Ok(())
    }

    pub(super) fn close(self) {
        if !self.was_manual {
            unsafe {
                self.app.set_attribute_value(
                    &CFString::from_str("AXManualAccessibility"),
                    CFBoolean::new(false),
                );
            }
        }
    }

    fn fresh(&self, element: &Element) -> Result<CFRetained<AXUIElement>> {
        let target = self
            .handles
            .get(element.handle)
            .with_context(|| format!("{} is gone", element.describe()))?;
        let current = self
            .read(target)
            .map(|read| read.node(element.handle))
            .with_context(|| format!("{} is gone", element.describe()))?;
        if current.role != element.role || current.name != element.raw_name {
            bail!("{} changed before it could be used", element.describe());
        }
        Ok(target.clone())
    }

    fn set(&self, element: &AXUIElement, attribute: &str, value: &CFType) -> bool {
        unsafe {
            element.set_attribute_value(&CFString::from_str(attribute), value) == AXError::Success
        }
    }

    fn key(&self, code: u16, flags: CGEventFlags, character: Option<char>) {
        for down in [true, false] {
            let Some(event) = CGEvent::new_keyboard_event(None, code, down) else {
                continue;
            };
            CGEvent::set_flags(Some(&event), flags);
            if let Some(character) = character {
                let mut units = [0; 2];
                let units = character.encode_utf16(&mut units);
                unsafe {
                    CGEvent::keyboard_set_unicode_string(
                        Some(&event),
                        units.len() as _,
                        units.as_ptr(),
                    )
                };
            }
            CGEvent::post_to_pid(self.pid, Some(&event));
        }
    }

    fn read(&self, element: &AXUIElement) -> Option<Read> {
        let mut values: *const CFArray = std::ptr::null();
        let error = unsafe {
            element.copy_multiple_attribute_values(
                self.attributes.as_opaque(),
                AXCopyMultipleAttributeOptions(0),
                NonNull::from(&mut values),
            )
        };
        if error != AXError::Success {
            return None;
        }
        let values: CFRetained<CFArray<CFType>> =
            unsafe { CFRetained::from_raw(NonNull::new(values.cast_mut())?.cast()) };
        let at = |index: usize| values.get(index);
        let text = |index| {
            at(index)
                .and_then(|value| value.downcast::<CFString>().ok())
                .map(|text| text.to_string())
                .unwrap_or_default()
        };
        let flag = |index| {
            at(index)
                .and_then(|value| value.downcast::<CFBoolean>().ok())
                .map(|flag| flag.as_bool())
        };
        let empty = at(10)
            .and_then(|value| value.downcast::<AXValue>().ok())
            .and_then(|value| {
                let mut size = CGSize::default();
                unsafe { value.value(AXValueType::CGSize, NonNull::from(&mut size).cast()) }
                    .then_some(size)
            })
            .is_some_and(|size| size.width <= 0.0 || size.height <= 0.0);
        Some(Read {
            role: text(0),
            subrole: text(1),
            title: text(2),
            description: text(3),
            text: text(4),
            number: at(4)
                .and_then(|value| value.downcast::<CFNumber>().ok())
                .and_then(|number| number.as_i64()),
            enabled: flag(5),
            focused: flag(6),
            selected: flag(7),
            expanded: flag(8),
            placeholder: text(9),
            empty,
            children: at(11)
                .and_then(|value| value.downcast::<CFArray>().ok())
                .map(|children| {
                    unsafe { CFRetained::cast_unchecked::<CFArray<AXUIElement>>(children) }
                        .iter()
                        .collect()
                })
                .unwrap_or_default(),
        })
    }
}

impl Read {
    fn node(&self, handle: usize) -> Node {
        let (role, kind) = role(&self.role, &self.subrole);
        let secret = self.subrole == "AXSecureTextField";
        let mut value = if secret {
            String::new()
        } else {
            self.text.clone()
        };
        let mut name = [&self.title, &self.description, &self.placeholder]
            .into_iter()
            .find(|label| !label.is_empty())
            .cloned()
            .unwrap_or_default();
        if role == "text" && name.is_empty() {
            name = std::mem::take(&mut value);
        }
        let mut states = Vec::new();
        if matches!(role, "checkbox" | "switch" | "radio") {
            states.extend(match self.number {
                Some(0) => Some("unchecked"),
                Some(1) => Some("checked"),
                Some(2) => Some("mixed"),
                _ => None,
            });
        }
        for (flag, state) in [
            (self.enabled.map(|enabled| !enabled), "disabled"),
            (self.expanded, "expanded"),
            (self.selected, "selected"),
            (self.focused, "focused"),
        ] {
            if flag == Some(true) {
                states.push(state);
            }
        }
        if kind == Some(Kind::Type) && value.is_empty() && !secret {
            states.push("empty");
        }
        Node {
            role: role.into(),
            name,
            value,
            states,
            kind,
            handle,
            children: Vec::new(),
        }
    }
}

fn role(role: &str, subrole: &str) -> (&'static str, Option<Kind>) {
    match (role, subrole) {
        ("AXButton" | "AXMenuButton" | "AXDisclosureTriangle", _) => ("button", Some(Kind::Press)),
        ("AXLink", _) => ("link", Some(Kind::Press)),
        ("AXTextField" | "AXTextArea", "AXSearchField") => ("searchbox", Some(Kind::Type)),
        ("AXTextField" | "AXTextArea", _) => ("textbox", Some(Kind::Type)),
        ("AXComboBox", _) => ("combobox", Some(Kind::Type)),
        ("AXPopUpButton", _) => ("combobox", Some(Kind::Press)),
        ("AXCheckBox", "AXSwitch") => ("switch", Some(Kind::Press)),
        ("AXCheckBox", _) => ("checkbox", Some(Kind::Press)),
        ("AXRadioButton", "AXTabButton") | ("AXTab", _) => ("tab", Some(Kind::Press)),
        ("AXRadioButton", _) => ("radio", Some(Kind::Press)),
        ("AXMenuItem" | "AXMenuBarItem", _) => ("menuitem", Some(Kind::Press)),
        ("AXRow" | "AXOutlineRow", _) => ("row", Some(Kind::Press)),
        ("AXCell", _) => ("gridcell", Some(Kind::Press)),
        ("AXStaticText", _) => ("text", None),
        ("AXHeading", _) => ("heading", None),
        ("AXSheet", _) | ("AXWindow", "AXDialog") => ("dialog", None),
        ("AXWindow", _) => ("window", None),
        ("AXToolbar", _) => ("toolbar", None),
        ("AXGroup", _) => ("group", None),
        ("AXMenu", _) => ("menu", None),
        ("AXImage", _) => ("image", None),
        ("AXWebArea", _) => ("page", None),
        ("AXApplication", _) => ("application", None),
        _ => ("", None),
    }
}

fn switched_on(app: &AXUIElement, attribute: &CFString) -> bool {
    let mut value: *const CFType = std::ptr::null();
    let read = unsafe { app.copy_attribute_value(attribute, NonNull::from(&mut value)) };
    read == AXError::Success
        && NonNull::new(value.cast_mut()).is_some_and(|value| {
            let value: CFRetained<CFType> = unsafe { CFRetained::from_raw(value) };
            value
                .downcast::<CFBoolean>()
                .is_ok_and(|flag| flag.as_bool())
        })
}

fn next_limit(limit: usize, read: usize, took: Duration) -> usize {
    if took < WALK_WITHIN / 2 {
        MAX_NODES
    } else {
        limit.min(read)
    }
}

fn cut_short(nodes: &mut Vec<Node>) {
    if nodes.is_empty() {
        return;
    }
    nodes.push(Node {
        role: "text".into(),
        name: "(the rest of the app was not read)".into(),
        ..Node::default()
    });
    let note = nodes.len() - 1;
    nodes[0].children.push(note);
}

fn executables_of(app: &str) -> Result<String> {
    let folder = if app.contains('/') {
        format!("{}/Contents/MacOS/", std::fs::canonicalize(app)?.display())
    } else {
        format!("/{}.app/Contents/MacOS/", app.trim_end_matches(".app"))
    };
    Ok(folder.to_lowercase())
}

fn running() -> Vec<i32> {
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    let mut pids = vec![0; usize::try_from(count).unwrap_or_default() * 2];
    let bytes = i32::try_from(pids.len() * size_of::<i32>()).unwrap_or(i32::MAX);
    let listed = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    pids.truncate(usize::try_from(listed).unwrap_or_default());
    pids.sort_unstable();
    pids
}

fn executable(pid: i32) -> String {
    let mut path = vec![0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let length = unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
    path.truncate(usize::try_from(length).unwrap_or_default());
    String::from_utf8_lossy(&path).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_roles_map_to_the_browser_vocabulary() {
        assert_eq!(role("AXButton", ""), ("button", Some(Kind::Press)));
        assert_eq!(
            role("AXTextField", "AXSearchField"),
            ("searchbox", Some(Kind::Type))
        );
        assert_eq!(role("AXPopUpButton", ""), ("combobox", Some(Kind::Press)));
        assert_eq!(role("AXCheckBox", "AXSwitch").0, "switch");
        assert_eq!(role("AXScrollArea", ""), ("", None));
    }

    #[test]
    fn a_slow_walk_keeps_its_cut_until_the_app_is_fast_again() {
        assert_eq!(next_limit(MAX_NODES, 300, WALK_WITHIN), 300);
        assert_eq!(next_limit(300, 300, WALK_WITHIN * 3 / 4), 300);
        assert_eq!(next_limit(300, 300, Duration::from_millis(50)), MAX_NODES);
    }

    #[test]
    fn a_walk_cut_short_says_so_on_the_screen() {
        let mut nodes = vec![Node {
            role: "application".into(),
            name: "Big".into(),
            ..Node::default()
        }];
        cut_short(&mut nodes);
        let screen = Screen::new(&nodes, &[]);
        assert!(
            screen.text.contains("the rest of the app was not read"),
            "{}",
            screen.text
        );
    }

    #[test]
    fn an_app_is_found_by_its_main_executable_in_any_case() {
        let folder = executables_of("visual studio code").unwrap();
        let found = |path: &str| path.to_lowercase().contains(&folder);
        assert!(found(
            "/Applications/Visual Studio Code.app/Contents/MacOS/Code"
        ));
        assert!(!found(
            "/Applications/Visual Studio Code.app/Contents/Frameworks/Code Helper.app/Contents/MacOS/Code Helper"
        ));
    }

    #[test]
    fn labels_states_and_secrets_follow_the_attributes() {
        let checkbox = Read {
            role: "AXCheckBox".into(),
            description: "Enabled".into(),
            number: Some(1),
            focused: Some(true),
            ..Read::default()
        };
        let node = checkbox.node(3);
        assert_eq!(
            (node.name.as_str(), node.states.clone()),
            ("Enabled", vec!["checked", "focused"])
        );
        let secret = Read {
            role: "AXTextField".into(),
            subrole: "AXSecureTextField".into(),
            placeholder: "Password".into(),
            text: "hunter2".into(),
            ..Read::default()
        };
        let node = secret.node(4);
        assert_eq!((node.name.as_str(), node.value.as_str()), ("Password", ""));
        assert!(!node.states.contains(&"empty"));
        let text = Read {
            role: "AXStaticText".into(),
            text: "count 0".into(),
            ..Read::default()
        };
        assert_eq!(text.node(5).name, "count 0");
    }
}
