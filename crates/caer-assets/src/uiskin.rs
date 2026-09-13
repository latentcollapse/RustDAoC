//! UI skin loader: the client's own window templates, parsed into typed layout data.
//!
//! **Do not hand-build a UI.** The client ships two complete, finished skins — `ui/atlantis` and
//! `ui/isles` — as ~98 XML files each, and they already describe every window the game has. The job
//! is to READ them, not to reinvent a HUD:
//!
//! ```text
//!   ui/uimain.xml         <Include> list -> every window file in load order
//!   ui/<skin>/assets.xml  <Font> name -> tga, and the shared image/atlas definitions
//!   ui/<skin>/*.xml       <WindowTemplate> -> controls (labels, images, status icons)
//! ```
//!
//! Controls address their art by TEMPLATE NAME (`dlg_sm_title_noresize`) rather than by pixel rect,
//! and text by FONT NAME (`arial11`) — both resolved through `assets.xml`. That indirection is the
//! whole reason the skins are swappable, so it is preserved here rather than flattened at load.
//!
//! ## On the XML
//!
//! These files are a small, regular subset: elements, text, no attributes we need beyond the root
//! `ID`, no namespaces, no CDATA. A full XML crate would be a dependency carried into the shipped
//! client for a format this constrained, so [`Element`] is a ~60-line reader. It is deliberately
//! strict about mismatched tags: a skin that silently half-parses would surface as windows missing
//! controls, which is far harder to diagnose than a parse error naming the tag.

use std::collections::HashMap;
use std::io;
use std::path::Path;

/// A parsed XML element: tag, text content, and children in document order.
///
/// Order matters — a window's controls are drawn in the order the file lists them, so a map keyed
/// by tag would lose the layering.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Element {
    pub tag: String,
    pub text: String,
    pub children: Vec<Element>,
}

impl Element {
    /// First direct child with this tag.
    #[must_use]
    pub fn child(&self, tag: &str) -> Option<&Element> {
        self.children
            .iter()
            .find(|c| c.tag.eq_ignore_ascii_case(tag))
    }

    /// Every direct child with this tag, in document order.
    pub fn children_named<'a>(&'a self, tag: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.children
            .iter()
            .filter(move |c| c.tag.eq_ignore_ascii_case(tag))
    }

    /// Text of a named child, trimmed.
    #[must_use]
    pub fn text_of(&self, tag: &str) -> Option<&str> {
        self.child(tag)
            .map(|c| c.text.trim())
            .filter(|t| !t.is_empty())
    }

    /// Integer value of a named child. Missing and malformed both yield `None`, so callers can
    /// apply their own default rather than silently getting a zero.
    #[must_use]
    pub fn int_of(&self, tag: &str) -> Option<i32> {
        self.text_of(tag)?.parse().ok()
    }

    /// Boolean value of a named child. The skins write `true`/`false` for flags but `0`/`1` for the
    /// resize fields, so both spellings are accepted.
    #[must_use]
    pub fn bool_of(&self, tag: &str) -> Option<bool> {
        match self.text_of(tag)?.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Some(true),
            "false" | "0" | "no" => Some(false),
            _ => None,
        }
    }

    /// `<Position><X>..</X><Y>..</Y></Position>` — the layout primitive, defaulting to the origin
    /// because most controls that omit it sit at their parent's corner.
    #[must_use]
    pub fn position(&self) -> (i32, i32) {
        let p = self.child("Position");
        let get = |t: &str| p.and_then(|p| p.int_of(t)).unwrap_or(0);
        (get("X"), get("Y"))
    }

    /// An `<X>/<Y>` pair — the shape `Size`, `TopLeft` and every nine-slice patch corner share.
    #[must_use]
    pub fn xy(&self) -> (i32, i32) {
        (self.int_of("X").unwrap_or(0), self.int_of("Y").unwrap_or(0))
    }

    /// `<Color><R/><G/><B/><A/></Color>`, defaulting to opaque white — an unstated colour means
    /// "draw the art as authored", not "draw it black".
    #[must_use]
    pub fn color(&self) -> Rgba {
        let c = self.child("Color");
        let get = |t: &str| c.and_then(|c| c.int_of(t)).unwrap_or(255).clamp(0, 255) as u8;
        Rgba {
            r: get("R"),
            g: get("G"),
            b: get("B"),
            a: get("A"),
        }
    }

    /// Parse a whole document, returning the root element.
    pub fn parse(xml: &str) -> io::Result<Element> {
        let b = xml.as_bytes();
        let mut i = 0usize;
        let mut stack: Vec<Element> = Vec::new();
        let mut root: Option<Element> = None;

        while i < b.len() {
            let Some(lt) = b[i..].iter().position(|&c| c == b'<').map(|p| p + i) else {
                break;
            };
            // Text belongs to the element currently open.
            if lt > i {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&xml[i..lt]);
                }
            }
            // Declarations, comments and doctypes carry nothing we need.
            if xml[lt..].starts_with("<?") || xml[lt..].starts_with("<!") {
                let end = if xml[lt..].starts_with("<!--") {
                    xml[lt..].find("-->").map(|p| lt + p + 3)
                } else {
                    xml[lt..].find('>').map(|p| lt + p + 1)
                };
                i = end.ok_or_else(|| err("unterminated declaration or comment"))?;
                continue;
            }
            let Some(gt) = b[lt..].iter().position(|&c| c == b'>').map(|p| p + lt) else {
                return Err(err("unterminated tag"));
            };
            let inner = xml[lt + 1..gt].trim();
            i = gt + 1;

            if let Some(name) = inner.strip_prefix('/') {
                // Closing tag: it must match what is actually open.
                let name = name.trim();
                let done = stack
                    .pop()
                    .ok_or_else(|| err(&format!("closing </{name}> with nothing open")))?;
                if !done.tag.eq_ignore_ascii_case(name) {
                    return Err(err(&format!(
                        "closing </{name}> does not match open <{}>",
                        done.tag
                    )));
                }
                match stack.last_mut() {
                    Some(parent) => parent.children.push(done),
                    None => root = Some(done),
                }
            } else {
                // Self-closing elements carry no content and no children.
                let (name, self_closing) = match inner.strip_suffix('/') {
                    Some(n) => (n.trim(), true),
                    None => (inner, false),
                };
                // Drop any attributes: the only one in these files is the root's `ID`.
                let name = name.split_whitespace().next().unwrap_or("").to_string();
                if name.is_empty() {
                    return Err(err("empty tag name"));
                }
                let el = Element {
                    tag: name,
                    ..Default::default()
                };
                if self_closing {
                    match stack.last_mut() {
                        Some(parent) => parent.children.push(el),
                        None => root = Some(el),
                    }
                } else {
                    stack.push(el);
                }
            }
        }
        if let Some(open) = stack.last() {
            return Err(err(&format!("unclosed <{}>", open.tag)));
        }
        root.ok_or_else(|| err("no root element"))
    }
}

/// Resolve a file name inside `dir`, falling back to a case-insensitive match.
///
/// The skins were authored on Windows, where the filesystem does not care: `uimain.xml` includes
/// `ChatRename.xml` while the file on disk is `chatrename.xml`. On Linux that window simply vanishes
/// — and a missing window is not an error anyone would connect back to letter case.
pub fn resolve_ignoring_case(dir: &Path, name: &str) -> std::path::PathBuf {
    let exact = dir.join(name);
    if exact.exists() {
        return exact;
    }
    // `name` may carry sub-directories (`fonts/arial11b.tga`), and ANY component can differ in
    // case, so walk it a component at a time rather than only fixing the file name.
    let mut at = dir.to_path_buf();
    for part in std::path::Path::new(name).components() {
        let want = part.as_os_str().to_string_lossy().into_owned();
        let next = at.join(&want);
        if next.exists() {
            at = next;
            continue;
        }
        let found = std::fs::read_dir(&at).ok().and_then(|rd| {
            rd.flatten()
                .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(&want))
                .map(|e| e.path())
        });
        match found {
            Some(p) => at = p,
            // Nothing matched: report the miss against the name that was asked for.
            None => return exact,
        }
    }
    at
}

fn err(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// A colour as the skins express it: 0–255 per channel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

/// A text control.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Label {
    pub control_id: Option<String>,
    pub color: Rgba,
    pub font: Option<String>,
    pub width: i32,
    pub height: i32,
    pub max_characters: i32,
    /// Authored placeholder text. Often a designer's filler ("Bizquit" in the clock window) that the
    /// adapter overwrites at runtime — so it is NOT something to display verbatim.
    pub data: Option<String>,
    pub pos: (i32, i32),
    /// The live value that fills this label (`time_of_day`, `player_name`, …). This is the binding
    /// point between the skin and the game state, and the reason the skins are data rather than art.
    pub adapter: Option<String>,
    pub center_horizontally: bool,
    /// Authored click handler name (`OnClickEvent` / `onClickEvent`), when present.
    pub click_event: Option<String>,
}

/// An image control: a rect filled from a named art template in `assets.xml`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Image {
    pub control_id: Option<String>,
    pub template_name: Option<String>,
    pub width: i32,
    pub height: i32,
    pub pos: (i32, i32),
}

/// One control inside a window, in draw order.
#[derive(Clone, Debug, PartialEq)]
pub enum Control {
    Label(Label),
    Image(Image),
    Button(Button),
    EditBox(EditBox),
    /// A control type we parse the geometry of but do not yet model (status icons, buttons, lists).
    /// Kept rather than dropped so a window's control COUNT stays honest and nothing silently
    /// disappears from a ported layout.
    Other {
        tag: String,
        control_id: Option<String>,
        pos: (i32, i32),
    },
}

/// One window from the skin.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindowTemplate {
    pub name: String,
    pub window_id: Option<String>,
    pub width: i32,
    pub height: i32,
    pub title_width: i32,
    pub title_height: i32,
    pub close_button: bool,
    pub move_button: bool,
    pub controls: Vec<Control>,
}

impl WindowTemplate {
    /// Build from a `<WindowTemplate>` element.
    #[must_use]
    pub fn from_element(el: &Element) -> Self {
        let mut w = WindowTemplate {
            name: el.text_of("Name").unwrap_or_default().to_string(),
            window_id: el.text_of("WindowId").map(str::to_string),
            width: el.int_of("Width").unwrap_or(0),
            height: el.int_of("Height").unwrap_or(0),
            title_width: el.int_of("TitleWidth").unwrap_or(0),
            title_height: el.int_of("TitleHeight").unwrap_or(0),
            close_button: el.bool_of("CloseButton").unwrap_or(false),
            move_button: el.bool_of("MoveButton").unwrap_or(false),
            controls: Vec::new(),
        };
        for c in &el.children {
            // Skip the window's own scalar fields; only control definitions become controls.
            let tag = c.tag.as_str();
            // `LabelDef` and `ScalarLabelDef` are both text controls — the scalar variant is how
            // the summary window binds the player's vitals, so treating it as "some other control"
            // silently dropped 159 labels and every adapter behind them.
            if tag.eq_ignore_ascii_case("LabelDef") || tag.eq_ignore_ascii_case("ScalarLabelDef") {
                w.controls.push(Control::Label(Label {
                    control_id: c.text_of("ControlId").map(str::to_string),
                    color: c.color(),
                    font: c.text_of("FontName").map(str::to_string),
                    width: c.int_of("Width").unwrap_or(0),
                    height: c.int_of("Height").unwrap_or(0),
                    max_characters: c.int_of("MaxCharacters").unwrap_or(0),
                    data: c.text_of("Data").map(str::to_string),
                    pos: c.position(),
                    adapter: c.text_of("Adapter").map(str::to_string),
                    center_horizontally: c
                        .child("Alignment")
                        .and_then(|a| a.bool_of("CenterHorizontally"))
                        .unwrap_or(false),
                    click_event: c
                        .text_of("OnClickEvent")
                        .or_else(|| c.text_of("onClickEvent"))
                        .map(str::to_string),
                }));
            } else if tag.eq_ignore_ascii_case("ButtonDef") {
                // A real control kind now. This arm used to flatten a button into a `Label` so its
                // caption at least appeared — honest at the time, and the reason the login dialog's
                // OK and QUIT are words with no art (J10). The art template was already modelled in
                // `Skin::buttons`; nothing was consulting it.
                w.controls.push(Control::Button(Button {
                    control_id: c.text_of("ControlId").map(str::to_string),
                    template_name: c.text_of("TemplateName").map(str::to_string),
                    label: c.text_of("Label").map(str::to_string),
                    pos: c.position(),
                    size: (
                        c.int_of("Width").unwrap_or(0),
                        c.int_of("Height").unwrap_or(0),
                    ),
                    center_horizontally: c
                        .child("Alignment")
                        .and_then(|a| a.bool_of("CenterHorizontally"))
                        .unwrap_or(true),
                    click_event: c
                        .text_of("OnClickEvent")
                        .or_else(|| c.text_of("onClickEvent"))
                        .map(str::to_string),
                }));
            } else if tag.eq_ignore_ascii_case("EditBoxDef")
                || tag.eq_ignore_ascii_case("TextAreaDef")
            {
                w.controls.push(Control::EditBox(EditBox {
                    control_id: c.text_of("ControlId").map(str::to_string),
                    template_name: c.text_of("TemplateName").map(str::to_string),
                    pos: c.position(),
                    size: (
                        c.int_of("Width").unwrap_or(0),
                        c.int_of("Height").unwrap_or(0),
                    ),
                    max_characters: c.int_of("MaxCharacters").unwrap_or(0),
                    // `AdapterName`, the same spelling the client uses on these two tags. A label
                    // spells it `Adapter`, which is why one shared reader would drop every field's
                    // binding.
                    adapter: c.text_of("AdapterName").map(str::to_string),
                    multiline: tag.eq_ignore_ascii_case("TextAreaDef"),
                }));
            } else if tag.to_ascii_lowercase().ends_with("imagedef") {
                // `ImageDef`, `FullResizeImageDef`, `ImageAreaDef` — all a rect plus an art template.
                w.controls.push(Control::Image(Image {
                    control_id: c.text_of("ControlId").map(str::to_string),
                    template_name: c.text_of("TemplateName").map(str::to_string),
                    width: c.int_of("Width").unwrap_or(0),
                    height: c.int_of("Height").unwrap_or(0),
                    pos: c.position(),
                }));
            } else if tag.to_ascii_lowercase().ends_with("def") {
                w.controls.push(Control::Other {
                    tag: c.tag.clone(),
                    control_id: c.text_of("ControlId").map(str::to_string),
                    pos: c.position(),
                });
            }
        }
        w
    }
}

/// A font the skin declares. The skins ship BOTH kinds and controls reference them by the same
/// name, so a loader that only reads `<Font>` silently loses every TrueType one — which is how
/// `chat_small`, `chat_large` and `minion` came back as "referenced but not declared".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Font {
    /// The `.tga` glyph atlas, or the `.ttf` file for a TrueType font.
    pub file: String,
    /// Point size, present only on `<TTFFont>` — a bitmap atlas is fixed at its authored size.
    pub height: Option<i32>,
}

/// A named texture page — the atlas every art template cuts its rects out of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Texture {
    /// `page2`, `page3`, … as templates refer to it.
    pub name: String,
    /// The `.tga` on disk, relative to the skin directory.
    pub file: String,
}

/// Which art a button is currently wearing.
///
/// Every `ButtonTemplate` authors all four, and a client that only ever draws `Normal` looks
/// broken in a specific way: nothing responds to the pointer and a chosen option is
/// indistinguishable from an unchosen one. That is exactly how the pre-world race and class
/// buttons read — the clicks were landing and dispatching the whole time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ButtonState {
    Normal,
    /// Pointer over the control.
    Highlit,
    /// Held down, or latched on for a radio-style control.
    Pressed,
    Disabled,
}

/// A `ButtonTemplate` from `styles.xml`: one atlas, four state crops, four label colours.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ButtonTemplate {
    pub name: String,
    /// Texture page NAME (`misc_controls_new`), resolved to a file through [`Skin::texture`].
    pub texture: String,
    /// Crop size, shared by every state.
    pub size: (i32, i32),
    /// Top-left of each state's crop in the atlas.
    pub normal: (i32, i32),
    pub highlit: (i32, i32),
    pub pressed: (i32, i32),
    pub disabled: (i32, i32),
    /// Label font name, and its colour per state.
    pub font: String,
    pub color_normal: Rgba,
    pub color_highlit: Rgba,
    pub color_pressed: Rgba,
    pub color_disabled: Rgba,
}

impl ButtonTemplate {
    /// Atlas crop for a state, as `(x, y, w, h)`.
    #[must_use]
    pub fn crop(&self, state: ButtonState) -> (i32, i32, i32, i32) {
        let (x, y) = match state {
            ButtonState::Normal => self.normal,
            ButtonState::Highlit => self.highlit,
            ButtonState::Pressed => self.pressed,
            ButtonState::Disabled => self.disabled,
        };
        (x, y, self.size.0, self.size.1)
    }

    /// Label colour for a state.
    #[must_use]
    pub fn color(&self, state: ButtonState) -> Rgba {
        match state {
            ButtonState::Normal => self.color_normal,
            ButtonState::Highlit => self.color_highlit,
            ButtonState::Pressed => self.color_pressed,
            ButtonState::Disabled => self.color_disabled,
        }
    }

    fn from_element(el: &Element) -> Self {
        let tex = el.child("Texture");
        let at = |tag: &str| tex.and_then(|t| t.child(tag)).map_or((0, 0), Element::xy);
        let font = el.child("Font");
        // `<ColorNormal><R/><G/><B/><A/></ColorNormal>` holds the channels directly, unlike
        // `Element::color`, which expects them wrapped in a `<Color>` child. Using that here would
        // have silently returned white for all four states — every button the same colour, which
        // is indistinguishable from the bug being fixed.
        let color = |tag: &str| {
            font.and_then(|f| f.child(tag)).map_or(
                Rgba {
                    r: 255,
                    g: 255,
                    b: 255,
                    a: 255,
                },
                |c| {
                    let ch = |t: &str| c.int_of(t).unwrap_or(255).clamp(0, 255) as u8;
                    Rgba {
                        r: ch("R"),
                        g: ch("G"),
                        b: ch("B"),
                        a: ch("A"),
                    }
                },
            )
        };
        Self {
            name: el.text_of("Name").unwrap_or_default().to_string(),
            texture: tex
                .and_then(|t| t.text_of("TextureName"))
                .unwrap_or_default()
                .to_string(),
            size: el.child("Size").map_or((0, 0), Element::xy),
            normal: at("Normal"),
            // `PressedHighlit` duplicates `Pressed` in every pregame template, so it is not modelled
            // separately until a skin is found that distinguishes them.
            highlit: at("NormalHighlit"),
            pressed: at("Pressed"),
            disabled: at("Disabled"),
            font: font
                .and_then(|f| f.text_of("Name"))
                .unwrap_or_default()
                .to_string(),
            color_normal: color("ColorNormal"),
            color_highlit: color("ColorHighlit"),
            color_pressed: color("ColorPressed"),
            color_disabled: color("ColorDisabled"),
        }
    }
}

/// A flat rect cut from a texture page (`ImageAreaTemplate`) — an icon, an end-cap, a divider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageArea {
    pub name: String,
    pub texture: String,
    /// Rect size in pixels.
    pub size: (i32, i32),
    /// Top-left corner within the texture page.
    pub top_left: (i32, i32),
}

/// An `EditBoxTemplate` / `TextAreaTemplate` — where a field's text sits inside its rect.
///
/// **There is no frame art here, and that is the client's decision, not a gap in ours.** Every
/// pre-world edit box uses `generic_editbox`, which declares
/// `<BackgroundTemplate>none</BackgroundTemplate>`: retail draws no box, only the text and a caret
/// inside the authored rect. Ledger A15 nearly went looking for a nine-slice to decode that does not
/// exist. What the template *does* own is [`Self::text_offset`], and using it is the difference
/// between text inside the field and text jammed against its corner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditBoxTemplate {
    pub name: String,
    /// `TextOffset` — where the text starts relative to the control's top-left.
    pub text_offset: (i32, i32),
    /// Font name, and the colours for an idle and a focused field.
    pub font: String,
    pub color_normal: Rgba,
    pub color_active: Rgba,
}

/// A `ButtonDef` — an art template, a caption, and where it sits.
///
/// **Its size is deliberately not stored.** A `ButtonDef` almost never authors `Width`/`Height`; the
/// size belongs to the [`ButtonTemplate`] it names, and the template lives in `styles.xml`, which may
/// be absorbed after the window that uses it. Resolving the size at layout time is what avoids the
/// defect this type replaced: `ButtonDef` used to be flattened into a [`Label`] with a **default
/// 80x24**, which drew the caption with no art and gave every button on the login dialog a hit rect
/// 16px wider and 3px taller than the 64x21 its template declares (ledger J10).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Button {
    pub control_id: Option<String>,
    /// The `ButtonTemplate` name (`button_small`), lowercased at lookup.
    pub template_name: Option<String>,
    /// The authored caption. Absent for an art-only button.
    pub label: Option<String>,
    pub pos: (i32, i32),
    /// Authored size, when the form overrides its template. `(0, 0)` means "use the template".
    pub size: (i32, i32),
    pub center_horizontally: bool,
    pub click_event: Option<String>,
}

/// An `EditBoxDef` / `TextAreaDef` — a field the player types into.
///
/// It carries a real rect, unlike [`Button`]: an edit box **does** author `Width`/`Height`, because
/// its size is the space for text rather than a piece of art. Ledger J10 recorded these as "not
/// rendered at all"; they were parsed into `Control::Other`, counted, and never drawn — so the login
/// dialog had nowhere visible to type even once the account was known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditBox {
    pub control_id: Option<String>,
    /// The `EditBoxTemplate` name (`generic_editbox`).
    pub template_name: Option<String>,
    pub pos: (i32, i32),
    pub size: (i32, i32),
    pub max_characters: i32,
    /// The live binding this field reads (`username_text`, `password_text`, `name_edit`).
    pub adapter: Option<String>,
    /// True for a `TextAreaDef`, which wraps; an `EditBoxDef` is one line.
    pub multiline: bool,
}

/// A NINE-SLICE frame (`FullResizeImageTemplate`) — how every resizable window is drawn.
///
/// The corners are kept fixed, the edges stretch along one axis, and the middle stretches both
/// ways. That is why one small atlas region can dress a window at any size, and why these must not
/// be flattened into a single rect: stretching the whole image would smear the border art.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NineSlice {
    pub name: String,
    pub texture: String,
    /// Row heights: the fixed top band, the stretchable middle, the fixed bottom band.
    pub top_height: i32,
    pub middle_height: i32,
    pub bottom_height: i32,
    /// Column widths: fixed left, stretchable middle, fixed right.
    pub left_width: i32,
    pub middle_width: i32,
    pub right_width: i32,
    /// Top-left of each of the nine patches within the texture page, in reading order:
    /// TopLeft, TopMiddle, TopRight, MiddleLeft, MiddleMiddle, MiddleRight,
    /// BottomLeft, BottomMiddle, BottomRight.
    pub patches: [(i32, i32); 9],
}

/// The nine patch names, in the order [`NineSlice::patches`] stores them.
pub const NINE_SLICE_PATCHES: [&str; 9] = [
    "TopLeft",
    "TopMiddle",
    "TopRight",
    "MiddleLeft",
    "MiddleMiddle",
    "MiddleRight",
    "BottomLeft",
    "BottomMiddle",
    "BottomRight",
];

/// A whole loaded skin: its fonts and its windows.
#[derive(Clone, Debug, Default)]
pub struct Skin {
    /// Font name, LOWERCASED → the font. Lowercased because the skins are inconsistent about case
    /// (`assets.xml` declares `arial11`, windows ask for `Arial11`) and were authored on a
    /// case-insensitive filesystem, so an exact-match lookup drops real fonts. Use [`Skin::font`].
    pub fonts: HashMap<String, Font>,
    /// Window name (`clock`) → its template.
    pub windows: HashMap<String, WindowTemplate>,
    /// Texture page name (`page3`) → its file, keyed lowercase like `fonts`.
    pub textures: HashMap<String, Texture>,
    /// Art template name (`sm_horiz_endcap_left`) → a flat atlas rect, keyed lowercase.
    pub image_areas: HashMap<String, ImageArea>,
    /// Art template name (`dlg_sm_title_noresize`) → a nine-slice frame, keyed lowercase.
    pub nine_slices: HashMap<String, NineSlice>,
    /// Button template name (`button_pregame_medium`) → its four state crops, keyed lowercase.
    pub buttons: HashMap<String, ButtonTemplate>,
    /// Edit-box / text-area template name (`generic_editbox`) → its text placement, keyed lowercase.
    pub edit_boxes: HashMap<String, EditBoxTemplate>,
    /// Template kinds we parse the NAME of but do not yet model (buttons, list boxes, sliders),
    /// counted by tag. Kept so the skin's template inventory stays honest rather than looking
    /// complete because the unmodelled ones vanished.
    pub unmodelled_templates: HashMap<String, usize>,
    /// Files named by `uimain.xml` that could not be read or parsed, with the reason. Reported
    /// rather than fatal: one bad window should not cost the player their entire interface.
    pub problems: Vec<String>,
}

impl Skin {
    /// Look a font up by the name a control asks for, ignoring case.
    #[must_use]
    pub fn font(&self, name: &str) -> Option<&Font> {
        self.fonts.get(&name.to_ascii_lowercase())
    }

    /// Look up a flat art rect by template name, ignoring case.
    #[must_use]
    pub fn image_area(&self, name: &str) -> Option<&ImageArea> {
        self.image_areas.get(&name.to_ascii_lowercase())
    }

    /// Look up a nine-slice frame by template name, ignoring case.
    #[must_use]
    pub fn nine_slice(&self, name: &str) -> Option<&NineSlice> {
        self.nine_slices.get(&name.to_ascii_lowercase())
    }

    /// Look an edit-box / text-area template up by the name a control asks for, ignoring case.
    #[must_use]
    pub fn edit_box(&self, name: &str) -> Option<&EditBoxTemplate> {
        self.edit_boxes.get(&name.to_ascii_lowercase())
    }

    /// Look up a button template by name, ignoring case.
    #[must_use]
    pub fn button(&self, name: &str) -> Option<&ButtonTemplate> {
        self.buttons.get(&name.to_ascii_lowercase())
    }

    /// Look up a texture page by name, ignoring case.
    #[must_use]
    pub fn texture(&self, name: &str) -> Option<&Texture> {
        self.textures.get(&name.to_ascii_lowercase())
    }

    /// Collect every `<WindowTemplate>` and font declaration anywhere in a document.
    ///
    /// Recursive because the skins are not consistent about nesting depth — most windows sit
    /// directly under the root, but some files wrap them, and a fixed-depth search would silently
    /// miss those.
    pub fn absorb(&mut self, root: &Element) {
        if root.tag.eq_ignore_ascii_case("WindowTemplate") {
            let w = WindowTemplate::from_element(root);
            if !w.name.is_empty() {
                self.windows.insert(w.name.clone(), w);
            }
            return;
        }
        // Both font element types, keyed case-insensitively.
        if root.tag.eq_ignore_ascii_case("Font") || root.tag.eq_ignore_ascii_case("TTFFont") {
            if let (Some(n), Some(f)) = (root.text_of("Name"), root.text_of("File")) {
                self.fonts.insert(
                    n.to_ascii_lowercase(),
                    Font {
                        file: f.to_string(),
                        height: root.int_of("Height"),
                    },
                );
            }
            return;
        }
        // `<Texture>` appears BOTH as a top-level page declaration in assets.xml (Name + File) and
        // as a nested block inside an art template (TextureName + patch coordinates). Only the
        // former is a page; distinguishing them by their children rather than by position keeps
        // this robust to where a skin chooses to declare things.
        if root.tag.eq_ignore_ascii_case("Texture") {
            if let (Some(n), Some(f)) = (root.text_of("Name"), root.text_of("File")) {
                self.textures.insert(
                    n.to_ascii_lowercase(),
                    Texture {
                        name: n.to_string(),
                        file: f.to_string(),
                    },
                );
                return;
            }
        }
        // `CheckBoxTemplate` is a `ButtonTemplate` in everything but its tag: same `Size`, same
        // four `Font` colours, same five-state `Texture` block. The Options Menu draws its checks
        // and radios from `generic_check`, so it goes in the same map rather than getting a
        // parallel type that would need a parallel draw path.
        if root.tag.eq_ignore_ascii_case("ButtonTemplate")
            || root.tag.eq_ignore_ascii_case("CheckBoxTemplate")
        {
            let b = ButtonTemplate::from_element(root);
            if !b.name.is_empty() && b.size != (0, 0) {
                self.buttons.insert(b.name.to_ascii_lowercase(), b);
                return;
            }
            // A name-only stub (some skins declare then override) stays counted as unmodelled
            // rather than inserted as a zero-sized button that would draw nothing.
        }
        if root.tag.eq_ignore_ascii_case("ImageAreaTemplate") {
            if let Some(name) = root.text_of("Name") {
                let size = root.child("Size").map_or((0, 0), Element::xy);
                self.image_areas.insert(
                    name.to_ascii_lowercase(),
                    ImageArea {
                        name: name.to_string(),
                        texture: root.text_of("TextureName").unwrap_or_default().to_string(),
                        size,
                        top_left: root.child("TopLeft").map_or((0, 0), Element::xy),
                    },
                );
            }
            return;
        }
        if root.tag.eq_ignore_ascii_case("FullResizeImageTemplate") {
            if let (Some(name), Some(tex)) = (root.text_of("Name"), root.child("Texture")) {
                let mut patches = [(0, 0); 9];
                for (i, p) in NINE_SLICE_PATCHES.iter().enumerate() {
                    patches[i] = tex.child(p).map_or((0, 0), Element::xy);
                }
                self.nine_slices.insert(
                    name.to_ascii_lowercase(),
                    NineSlice {
                        name: name.to_string(),
                        texture: tex.text_of("TextureName").unwrap_or_default().to_string(),
                        top_height: root.int_of("TopHeight").unwrap_or(0),
                        middle_height: root.int_of("MiddleHeight").unwrap_or(0),
                        bottom_height: root.int_of("BottomHeight").unwrap_or(0),
                        left_width: root.int_of("LeftWidth").unwrap_or(0),
                        middle_width: root.int_of("MiddleWidth").unwrap_or(0),
                        right_width: root.int_of("RightWidth").unwrap_or(0),
                        patches,
                    },
                );
            }
            return;
        }
        if root.tag.eq_ignore_ascii_case("EditBoxTemplate")
            || root.tag.eq_ignore_ascii_case("TextAreaTemplate")
        {
            if let Some(name) = root.text_of("Name") {
                let font = root.child("Font");
                let color = |tag: &str, fallback: u8| {
                    font.and_then(|f| f.child(tag)).map_or(
                        Rgba {
                            r: fallback,
                            g: fallback,
                            b: fallback,
                            a: 255,
                        },
                        |c| {
                            let get = |t: &str| c.int_of(t).unwrap_or(255).clamp(0, 255) as u8;
                            Rgba {
                                r: get("R"),
                                g: get("G"),
                                b: get("B"),
                                a: get("A"),
                            }
                        },
                    )
                };
                self.edit_boxes.insert(
                    name.to_ascii_lowercase(),
                    EditBoxTemplate {
                        name: name.to_string(),
                        text_offset: root.child("TextOffset").map_or((0, 0), Element::xy),
                        font: font
                            .and_then(|f| f.text_of("Name"))
                            .unwrap_or("button_large")
                            .to_string(),
                        color_normal: color("ColorNormal", 192),
                        color_active: color("ColorActive", 255),
                    },
                );
            }
            return;
        }
        // Everything else ending in `Template` is art we have not modelled yet — count it so the
        // inventory does not look complete just because the rest silently vanished.
        if root.tag.len() > 8 && root.tag.to_ascii_lowercase().ends_with("template") {
            *self
                .unmodelled_templates
                .entry(root.tag.clone())
                .or_default() += 1;
            return;
        }

        for c in &root.children {
            self.absorb(c);
        }
    }

    /// Load a skin: `uimain.xml`'s include list, resolved against the skin directory, plus the
    /// skin's own `assets.xml`.
    ///
    /// `ui_dir` is the client's `ui/` directory and `skin` is a subdirectory of it
    /// (`atlantis`/`isles`). Includes are named without a directory, so they resolve inside the
    /// skin — which is exactly what makes the two skins interchangeable.
    pub fn load(ui_dir: impl AsRef<Path>, skin: &str) -> io::Result<Self> {
        let ui_dir = ui_dir.as_ref();
        let skin_dir = ui_dir.join(skin);
        let mut out = Skin::default();

        // Fonts and shared art first, so a window referring to them resolves.
        let assets = resolve_ignoring_case(&skin_dir, "assets.xml");
        match std::fs::read_to_string(&assets).map(|t| Element::parse(&t)) {
            Ok(Ok(root)) => out.absorb(&root),
            Ok(Err(e)) => out.problems.push(format!("assets.xml: {e}")),
            Err(e) => out.problems.push(format!("assets.xml: {e}")),
        }

        let main = std::fs::read_to_string(resolve_ignoring_case(ui_dir, "uimain.xml"))?;
        let root = Element::parse(&main)?;
        for inc in root.children_named("Include") {
            let file = inc.text.trim();
            if file.is_empty() {
                continue;
            }
            let path = resolve_ignoring_case(&skin_dir, file);
            match std::fs::read_to_string(&path).map(|t| Element::parse(&t)) {
                Ok(Ok(doc)) => out.absorb(&doc),
                Ok(Err(e)) => out.problems.push(format!("{file}: {e}")),
                Err(e) => out.problems.push(format!("{file}: {e}")),
            }
        }
        Ok(out)
    }

    /// Atlantis base plus Ghost (`custom/`) override — the shipped in-world skin (DIRECTIVE §10A).
    ///
    /// Ghost windows replace Atlantis entries of the same `<Name>`. Missing `custom/` is not fatal:
    /// the Atlantis base still loads and the miss is recorded in [`Skin::problems`].
    pub fn load_shipped(ui_dir: impl AsRef<Path>) -> io::Result<Self> {
        let ui_dir = ui_dir.as_ref();
        let mut skin = Self::load(ui_dir, "atlantis")?;
        skin.overlay_skin(ui_dir, "custom");
        Ok(skin)
    }

    /// Merge another skin directory's `assets.xml` and `uimain.xml` includes on top of `self`.
    /// Later windows win. Multi-segment includes (`Options/…`) walk each path component
    /// case-insensitively via [`resolve_ignoring_case`].
    pub fn overlay_skin(&mut self, ui_dir: impl AsRef<Path>, skin: &str) {
        let ui_dir = ui_dir.as_ref();
        let skin_dir = ui_dir.join(skin);
        if !skin_dir.is_dir() {
            self.problems
                .push(format!("{skin}/: overlay directory missing"));
            return;
        }

        let assets = resolve_ignoring_case(&skin_dir, "assets.xml");
        match std::fs::read_to_string(&assets).map(|t| Element::parse(&t)) {
            Ok(Ok(root)) => self.absorb(&root),
            Ok(Err(e)) => self.problems.push(format!("{skin}/assets.xml: {e}")),
            Err(e) => self.problems.push(format!("{skin}/assets.xml: {e}")),
        }

        let main_path = resolve_ignoring_case(&skin_dir, "uimain.xml");
        match std::fs::read_to_string(&main_path).map(|t| Element::parse(&t)) {
            Ok(Ok(root)) => {
                for inc in root.children_named("Include") {
                    let file = inc.text.trim();
                    if file.is_empty() {
                        continue;
                    }
                    let path = resolve_ignoring_case(&skin_dir, file);
                    match std::fs::read_to_string(&path).map(|t| Element::parse(&t)) {
                        Ok(Ok(doc)) => self.absorb(&doc),
                        Ok(Err(e)) => self.problems.push(format!("{skin}/{file}: {e}")),
                        Err(e) => self.problems.push(format!("{skin}/{file}: {e}")),
                    }
                }
            }
            Ok(Err(e)) => self.problems.push(format!("{skin}/uimain.xml: {e}")),
            Err(e) => self.problems.push(format!("{skin}/uimain.xml: {e}")),
        }
    }

    /// Absorb one extra XML document (e.g. `pregame/login.xml`) into this skin's window map.
    ///
    /// Art templates / fonts stay from the skin; only `<WindowTemplate>`s are merged. Soft-fails
    /// into `problems` so a missing pregame file does not kill the HUD.
    pub fn absorb_file(&mut self, path: impl AsRef<Path>) {
        let path = path.as_ref();
        let label = path.display().to_string();
        match std::fs::read_to_string(path).map(|t| Element::parse(&t)) {
            Ok(Ok(doc)) => self.absorb(&doc),
            Ok(Err(e)) => self.problems.push(format!("{label}: {e}")),
            Err(e) => self.problems.push(format!("{label}: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact shape of `clock_window.xml`, which exercises the parts every window uses: sizes,
    /// flags, a background image addressed by art template, and an adapter-driven label.
    const CLOCK: &str = r#"<?xml version="1.0" encoding="ISO-8859-1"?>
<Root_Element ID="DAOCUi">
	<WindowTemplate>
		<Name>clock</Name>
		<CloseButton>true</CloseButton>
		<MoveButton>true</MoveButton>
		<Width>120</Width>
		<Height>40</Height>
		<TitleWidth>120</TitleWidth>
		<TitleHeight>18</TitleHeight>
		<ResizeableWidth>0</ResizeableWidth>
		<WindowId>Clock</WindowId>
		<FullResizeImageDef>
			<ControlId>Background</ControlId>
			<Height>40</Height>
			<Width>120</Width>
			<Position><X>0</X><Y>0</Y></Position>
			<TemplateName>dlg_sm_title_noresize</TemplateName>
		</FullResizeImageDef>
		<LabelDef>
			<Color><R>255</R><G>255</G><B>0</B><A>255</A></Color>
			<FontName>arial11</FontName>
			<Width>120</Width>
			<Height>16</Height>
			<MaxCharacters>128</MaxCharacters>
			<Data>Bizquit</Data>
			<Position><X>0</X><Y>18</Y></Position>
			<Adapter>time_of_day</Adapter>
			<Alignment><CenterHorizontally>true</CenterHorizontally></Alignment>
		</LabelDef>
	</WindowTemplate>
</Root_Element>"#;

    #[test]
    fn parses_a_real_window_template() {
        let root = Element::parse(CLOCK).expect("clock window parses");
        let mut skin = Skin::default();
        skin.absorb(&root);

        let w = skin
            .windows
            .get("clock")
            .expect("window registered under its Name");
        assert_eq!(w.window_id.as_deref(), Some("Clock"));
        assert_eq!((w.width, w.height), (120, 40));
        assert_eq!((w.title_width, w.title_height), (120, 18));
        assert!(
            w.close_button && w.move_button,
            "both buttons are `true` in this file"
        );
        // Two controls, in DOCUMENT order — the background must precede the label it sits behind.
        assert_eq!(w.controls.len(), 2, "{:?}", w.controls);

        let Control::Image(bg) = &w.controls[0] else {
            panic!("first control should be the image")
        };
        assert_eq!(bg.control_id.as_deref(), Some("Background"));
        assert_eq!(
            bg.template_name.as_deref(),
            Some("dlg_sm_title_noresize"),
            "art is addressed by TEMPLATE, not a rect"
        );
        assert_eq!((bg.width, bg.height), (120, 40));

        let Control::Label(l) = &w.controls[1] else {
            panic!("second control should be the label")
        };
        assert_eq!(l.font.as_deref(), Some("arial11"));
        assert_eq!(
            l.color,
            Rgba {
                r: 255,
                g: 255,
                b: 0,
                a: 255
            }
        );
        assert_eq!(l.pos, (0, 18));
        assert_eq!(l.max_characters, 128);
        assert!(l.center_horizontally, "nested Alignment must be read");
        // The binding point: this label is driven by game state, not by its authored placeholder.
        assert_eq!(l.adapter.as_deref(), Some("time_of_day"));
        assert_eq!(
            l.data.as_deref(),
            Some("Bizquit"),
            "authored filler is kept but is not display text"
        );
        assert_eq!(l.click_event, None);
    }

    /// Ghost/custom overlay replaces Atlantis windows of the same name (later absorb wins).
    #[test]
    fn overlay_skin_replaces_same_named_windows() {
        let mut skin = Skin::default();
        skin.absorb(
            &Element::parse(
                "<Root_Element><WindowTemplate><Name>summary</Name><Width>10</Width></WindowTemplate></Root_Element>",
            )
            .unwrap(),
        );
        assert_eq!(skin.windows.get("summary").unwrap().width, 10);
        skin.absorb(
            &Element::parse(
                "<Root_Element><WindowTemplate><Name>summary</Name><Width>99</Width></WindowTemplate></Root_Element>",
            )
            .unwrap(),
        );
        assert_eq!(
            skin.windows.get("summary").unwrap().width,
            99,
            "Ghost/custom override must replace Atlantis by window Name"
        );
    }

    #[test]
    fn button_def_keeps_onclick_event() {
        let xml = r#"<WindowTemplate>
            <Name>w</Name>
            <ButtonDef>
                <ControlId>ok</ControlId>
                <Label>OK</Label>
                <Width>40</Width>
                <Height>20</Height>
                <OnClickEvent>Accept</OnClickEvent>
                <Position><X>4</X><Y>8</Y></Position>
            </ButtonDef>
        </WindowTemplate>"#;
        let w = WindowTemplate::from_element(&Element::parse(xml).unwrap());
        // A button is its own control kind now. This test used to assert the opposite — "button must
        // remain a visible label" — which was the honest state of things while no art template was
        // modelled, and was also ledger J10: a `Label` cannot carry art, and the flattening's
        // default 80x24 became the hit rect of every button that did not author a size.
        let Control::Button(b) = &w.controls[0] else {
            panic!("a ButtonDef must parse as a Button, not a Label");
        };
        assert_eq!(b.control_id.as_deref(), Some("ok"));
        assert_eq!(b.click_event.as_deref(), Some("Accept"));
        assert_eq!(b.label.as_deref(), Some("OK"));
        assert_eq!(b.pos, (4, 8));
        // An authored size still wins over the template's.
        assert_eq!(b.size, (40, 20));
    }

    /// A `ButtonDef` with no `Width`/`Height` must report `(0, 0)`, not a guess.
    ///
    /// **Ledger J10.** The old parser defaulted these to `80x24`. `login.xml` authors neither on its
    /// OK or QUIT, whose `button_small` template is 64x21, so every login button responded 16px wider
    /// and 3px taller than the art the player could see. `(0, 0)` is what lets layout ask the
    /// template instead of inheriting a number nobody authored.
    #[test]
    fn a_button_without_an_authored_size_does_not_invent_one() {
        let xml = r#"<WindowTemplate>
            <Name>w</Name>
            <ButtonDef>
                <TemplateName>button_small</TemplateName>
                <ControlId>1001</ControlId>
                <Label>OK</Label>
                <Position><X>60</X><Y>100</Y></Position>
            </ButtonDef>
        </WindowTemplate>"#;
        let w = WindowTemplate::from_element(&Element::parse(xml).unwrap());
        let Control::Button(b) = &w.controls[0] else {
            panic!("ButtonDef must parse as a Button");
        };
        assert_eq!(
            b.size,
            (0, 0),
            "an unauthored size must stay unauthored, so layout resolves it from the template"
        );
        assert_eq!(b.template_name.as_deref(), Some("button_small"));
    }

    /// An `EditBoxDef` keeps its rect and its binding.
    ///
    /// **Ledger J10.** These fell through to `Control::Other`, which carries a position and nothing
    /// else — so the login dialog's two fields were counted and never drawn, and the player had
    /// nowhere visible to type. Note `AdapterName`, not `Adapter`: a label spells it the other way,
    /// and reading the wrong tag loses every field's binding while parsing cleanly.
    #[test]
    fn edit_box_def_keeps_its_rect_and_adapter() {
        let xml = r#"<WindowTemplate>
            <Name>w</Name>
            <EditBoxDef>
                <TemplateName>generic_editbox</TemplateName>
                <ControlId>1003</ControlId>
                <Position><X>125</X><Y>10</Y></Position>
                <Width>256</Width>
                <Height>32</Height>
                <MaxCharacters>32</MaxCharacters>
                <AdapterName>username_text</AdapterName>
            </EditBoxDef>
            <TextAreaDef>
                <TemplateName>generic_textarea</TemplateName>
                <ControlId>1054</ControlId>
                <Position><X>20</X><Y>134</Y></Position>
                <Width>220</Width>
                <Height>280</Height>
            </TextAreaDef>
        </WindowTemplate>"#;
        let w = WindowTemplate::from_element(&Element::parse(xml).unwrap());
        let Control::EditBox(e) = &w.controls[0] else {
            panic!("EditBoxDef must parse as an EditBox");
        };
        assert_eq!(e.control_id.as_deref(), Some("1003"));
        assert_eq!(e.template_name.as_deref(), Some("generic_editbox"));
        assert_eq!((e.pos, e.size), ((125, 10), (256, 32)));
        assert_eq!(e.max_characters, 32);
        assert_eq!(e.adapter.as_deref(), Some("username_text"));
        assert!(!e.multiline, "an EditBoxDef is one line");
        let Control::EditBox(a) = &w.controls[1] else {
            panic!("TextAreaDef must parse as an EditBox too");
        };
        assert!(a.multiline, "a TextAreaDef wraps");
        assert_eq!(a.adapter, None, "no AdapterName authored");
    }

    /// `generic_editbox`'s text placement comes from the template, and it has no frame art.
    ///
    /// The absent `BackgroundTemplate` is the client's decision (ledger A15): there is no nine-slice
    /// to decode, and retail draws no box either. `TextOffset` is the part that matters.
    #[test]
    fn edit_box_template_carries_text_offset_and_no_background() {
        let xml = r#"<Root_Element ID="DAOCUi">
            <EditBoxTemplate>
                <Name>generic_editbox</Name>
                <BackgroundTemplate>none</BackgroundTemplate>
                <TextOffset><X>10</X><Y>5</Y></TextOffset>
                <Font>
                    <Name>button_large</Name>
                    <ColorNormal><R>192</R><G>192</G><B>192</B><A>255</A></ColorNormal>
                    <ColorActive><R>255</R><G>255</G><B>255</B><A>255</A></ColorActive>
                </Font>
            </EditBoxTemplate>
        </Root_Element>"#;
        let mut skin = Skin::default();
        skin.absorb(&Element::parse(xml).unwrap());
        let t = skin.edit_box("Generic_EditBox").expect("case-insensitive");
        assert_eq!(t.text_offset, (10, 5));
        assert_eq!(t.font, "button_large");
        assert_eq!(t.color_normal.r, 192);
        assert_eq!(t.color_active.r, 255);
        assert!(
            !skin.unmodelled_templates.contains_key("EditBoxTemplate"),
            "EditBoxTemplate is modelled now and must stop being counted as a gap"
        );
    }

    #[test]
    fn reads_the_font_table() {
        let xml = r#"<Root_Element ID="DAOCUi">
            <Font><Name>arial9</Name><File>fonts/arial9.tga</File></Font>
            <Font><Name>brit9</Name><File>fonts/brit9gry_light.tga</File></Font>
        </Root_Element>"#;
        let mut skin = Skin::default();
        skin.absorb(&Element::parse(xml).unwrap());
        assert_eq!(skin.fonts.len(), 2);
        assert_eq!(
            skin.font("arial9").map(|f| f.file.as_str()),
            Some("fonts/arial9.tga")
        );
        // Controls ask with different capitalisation than assets.xml declares.
        assert_eq!(
            skin.font("Arial9").map(|f| f.file.as_str()),
            Some("fonts/arial9.tga")
        );
    }

    /// Both boolean spellings ship: flags use `true`/`false`, the resize fields use `0`/`1`.
    #[test]
    fn both_boolean_spellings_are_accepted() {
        let el =
            Element::parse("<R><a>true</a><b>false</b><c>1</c><d>0</d><e>maybe</e></R>").unwrap();
        assert_eq!(el.bool_of("a"), Some(true));
        assert_eq!(el.bool_of("b"), Some(false));
        assert_eq!(el.bool_of("c"), Some(true));
        assert_eq!(el.bool_of("d"), Some(false));
        assert_eq!(el.bool_of("e"), None, "nonsense must not read as false");
        assert_eq!(el.bool_of("missing"), None);
    }

    /// Defaults must be the ones that keep a control visible: white, not black; origin, not
    /// somewhere off-window.
    #[test]
    fn missing_colour_and_position_default_to_visible() {
        let el = Element::parse("<LabelDef><Width>10</Width></LabelDef>").unwrap();
        assert_eq!(
            el.color(),
            Rgba {
                r: 255,
                g: 255,
                b: 255,
                a: 255
            },
            "unstated colour draws as authored"
        );
        assert_eq!(el.position(), (0, 0));
        assert_eq!(el.int_of("Width"), Some(10));
        assert_eq!(
            el.int_of("Height"),
            None,
            "missing must be None, not a silent zero"
        );
    }

    /// Declarations, comments and self-closing tags must not derail the reader.
    #[test]
    fn handles_declarations_comments_and_self_closing_tags() {
        let xml =
            "<?xml version=\"1.0\"?><!-- a note --><R ID=\"x\"><A>1</A><B/><!--skip--><C>2</C></R>";
        let el = Element::parse(xml).unwrap();
        assert_eq!(el.tag, "R");
        assert_eq!(
            el.children.len(),
            3,
            "self-closing <B/> is a child too: {:?}",
            el.children
        );
        assert_eq!(el.int_of("A"), Some(1));
        assert_eq!(el.int_of("C"), Some(2));
    }

    /// A malformed skin must FAIL rather than half-parse. A window quietly missing its controls is
    /// far harder to diagnose than a parse error that names the tag.
    #[test]
    fn mismatched_and_unclosed_tags_are_errors() {
        let e = Element::parse("<A><B></C></A>").unwrap_err().to_string();
        assert!(e.contains("does not match"), "{e}");
        let e = Element::parse("<A><B></B>").unwrap_err().to_string();
        assert!(e.contains("unclosed"), "{e}");
        assert!(Element::parse("").is_err(), "an empty document has no root");
        assert!(Element::parse("<A><B>").is_err());
    }

    /// The nine-slice is how every resizable window frame is drawn: fixed corners, edges that
    /// stretch along one axis, a middle that stretches both. Flattening it to one rect would smear
    /// the border art at any size but the authored one.
    #[test]
    fn parses_a_nine_slice_frame() {
        let xml = r#"<Root_Element>
            <Texture><Name>page3</Name><File>atlantis_03.tga</File></Texture>
            <FullResizeImageTemplate>
                <Name>dlg_sm_title_noresize</Name>
                <TopHeight>20</TopHeight><MiddleHeight>10</MiddleHeight><BottomHeight>10</BottomHeight>
                <LeftWidth>20</LeftWidth><MiddleWidth>10</MiddleWidth><RightWidth>20</RightWidth>
                <Texture>
                    <TextureName>page3</TextureName>
                    <TopLeft><X>1</X><Y>193</Y></TopLeft>
                    <TopMiddle><X>22</X><Y>193</Y></TopMiddle>
                    <TopRight><X>33</X><Y>193</Y></TopRight>
                    <MiddleLeft><X>1</X><Y>214</Y></MiddleLeft>
                    <MiddleMiddle><X>22</X><Y>214</Y></MiddleMiddle>
                    <MiddleRight><X>33</X><Y>214</Y></MiddleRight>
                    <BottomLeft><X>1</X><Y>236</Y></BottomLeft>
                    <BottomMiddle><X>22</X><Y>236</Y></BottomMiddle>
                    <BottomRight><X>33</X><Y>236</Y></BottomRight>
                </Texture>
            </FullResizeImageTemplate>
        </Root_Element>"#;
        let mut skin = Skin::default();
        skin.absorb(&Element::parse(xml).unwrap());

        // The page declaration and the template's nested <Texture> must not be confused: one
        // declares a file, the other cuts rects out of it.
        assert_eq!(
            skin.texture("page3").map(|t| t.file.as_str()),
            Some("atlantis_03.tga")
        );
        assert_eq!(
            skin.textures.len(),
            1,
            "the nested <Texture> must not register as a page"
        );

        let n = skin
            .nine_slice("dlg_sm_title_noresize")
            .expect("frame registered");
        assert_eq!(n.texture, "page3");
        assert_eq!(
            (n.top_height, n.middle_height, n.bottom_height),
            (20, 10, 10)
        );
        assert_eq!((n.left_width, n.middle_width, n.right_width), (20, 10, 20));
        // Patches in reading order — corners first and last, so a transposed row would show.
        assert_eq!(n.patches[0], (1, 193), "TopLeft");
        assert_eq!(n.patches[4], (22, 214), "MiddleMiddle");
        assert_eq!(n.patches[8], (33, 236), "BottomRight");
        // Case-insensitive, like every other skin lookup.
        assert!(skin.nine_slice("DLG_SM_TITLE_NORESIZE").is_some());
    }

    /// A flat atlas rect, and the integrity property that matters: it names a real texture page.
    #[test]
    fn parses_an_image_area() {
        let xml = r#"<Root_Element>
            <ImageAreaTemplate>
                <Name>sm_horiz_endcap_left</Name>
                <TextureName>page2</TextureName>
                <Size><X>6</X><Y>6</Y></Size>
                <TopLeft><X>238</X><Y>29</Y></TopLeft>
            </ImageAreaTemplate>
        </Root_Element>"#;
        let mut skin = Skin::default();
        skin.absorb(&Element::parse(xml).unwrap());
        let a = skin.image_area("sm_horiz_endcap_left").expect("registered");
        assert_eq!(a.texture, "page2");
        assert_eq!(a.size, (6, 6));
        assert_eq!(a.top_left, (238, 29));
    }

    /// Template kinds we have not modelled are COUNTED, so the inventory cannot look complete
    /// merely because the rest disappeared.
    #[test]
    fn unmodelled_template_kinds_are_counted() {
        let xml = r#"<Root_Element>
            <ButtonTemplate><Name>window_close</Name></ButtonTemplate>
            <ButtonTemplate><Name>window_move</Name></ButtonTemplate>
            <ListBoxTemplate><Name>lb</Name></ListBoxTemplate>
        </Root_Element>"#;
        let mut skin = Skin::default();
        skin.absorb(&Element::parse(xml).unwrap());
        assert_eq!(skin.unmodelled_templates.get("ButtonTemplate"), Some(&2));
        assert_eq!(skin.unmodelled_templates.get("ListBoxTemplate"), Some(&1));
        assert!(skin.image_areas.is_empty() && skin.nine_slices.is_empty());
    }

    /// `ScalarLabelDef` is a text control too — it is how the summary window binds the player's
    /// vitals. Treating it as "some other control" silently dropped 159 labels across the skin and
    /// every adapter behind them, which showed up as an adapter demand of 151 instead of 282.
    #[test]
    fn scalar_labels_are_text_controls_too() {
        let xml = r#"<WindowTemplate>
            <Name>summary</Name>
            <ScalarLabelDef>
                <FontName>arial9</FontName>
                <Position><X>5</X><Y>22</Y></Position>
                <Adapter>summary_player_hits</Adapter>
            </ScalarLabelDef>
        </WindowTemplate>"#;
        let w = WindowTemplate::from_element(&Element::parse(xml).unwrap());
        assert_eq!(w.controls.len(), 1);
        let Control::Label(l) = &w.controls[0] else {
            panic!("expected a label, got {:?}", w.controls[0])
        };
        assert_eq!(l.adapter.as_deref(), Some("summary_player_hits"));
        assert_eq!(l.pos, (5, 22));
        assert_eq!(l.font.as_deref(), Some("arial9"));
    }

    /// Unmodelled control types are KEPT, so a ported window's control count stays honest instead
    /// of silently shedding the parts we haven't typed yet.
    #[test]
    fn unmodelled_controls_are_kept_not_dropped() {
        let xml = r#"<WindowTemplate>
            <Name>w</Name>
            <Width>10</Width>
            <StatusIconDef><ControlId>hp</ControlId><Position><X>4</X><Y>7</Y></Position></StatusIconDef>
        </WindowTemplate>"#;
        let w = WindowTemplate::from_element(&Element::parse(xml).unwrap());
        assert_eq!(w.controls.len(), 1);
        let Control::Other {
            tag,
            control_id,
            pos,
        } = &w.controls[0]
        else {
            panic!("{:?}", w.controls)
        };
        assert_eq!(tag, "StatusIconDef");
        assert_eq!(control_id.as_deref(), Some("hp"));
        assert_eq!(*pos, (4, 7));
        // Scalars like <Width> are window fields, NOT controls.
        assert_eq!(w.width, 10);
    }
}
