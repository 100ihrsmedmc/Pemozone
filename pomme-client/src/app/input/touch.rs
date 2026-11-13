//! Touch controls for gameplay, Bedrock-style: a move stick on the left,
//! dragging elsewhere to look, a tap to use, a still hold to destroy, and
//! buttons for jump, inventory and pause. Outside gameplay a touch is the
//! mouse pointer, so every screen works unchanged.
//!
//! Where each control sits is data ([`TouchLayoutSettings`]), not a constant:
//! a placement stores a normalized centre, a size multiplier and an opacity,
//! each optional so an absent value means the built-in position. The input
//! dispatch and the overlay draw both read the same [`TouchLayout`], so an
//! edited layout is honoured by both. The data-driven layout, the normalized
//! coordinates and the optional-override scheme follow Horizon's MIT-licensed
//! touch layout (`crates/horizon-client-render/src/touch/layout.rs`); the
//! code here is Pomme's own, written for its fixed-layout controls.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use glam::Vec2;
use serde::{Deserialize, Serialize};
use winit::event::TouchPhase;

use super::action::Action;

/// A press lifted within this, having stayed still, is a use.
const TAP_TIME: Duration = Duration::from_millis(250);
/// A press held still this long starts destroying.
const HOLD_TIME: Duration = Duration::from_millis(300);
/// How far a press may drift, in GUI units, and still count as still.
const STILL_SLOP: f32 = 4.0;
/// The stick's travel, in GUI units.
pub const STICK_RADIUS: f32 = 30.0;
/// Extra pixels that count as a hit on a control in the layout editor, so a
/// finger can grab a small button.
const EDIT_GRAB_SLOP: f32 = 12.0;
/// Mouse counts per GUI unit dragged.
const LOOK_SCALE: f32 = 2.5;
/// The share of the screen width, from the left, where a press is the stick.
const STICK_ZONE: f32 = 0.4;
pub const BUTTON_RADIUS: f32 = 14.0;
/// The close button drawn over an in-game screen, as a multiple of the button
/// radius: it rests where the pause button does, so the same corner closes
/// what the pause button opened, and it is a touch larger for an easy target.
pub const CLOSE_RADIUS: f32 = BUTTON_RADIUS * 1.5;

/// Where the controls sit on a `width` x `height` screen at GUI scale `gs`.
///
/// The built-in positions are the defaults; a [`TouchLayoutSettings`] may
/// override any control's centre, size and opacity. Everything an edit can
/// change lives in `settings`, so the input dispatch and the overlay draw see
/// the same geometry.
#[derive(Clone)]
pub struct TouchLayout {
    pub width: f32,
    pub height: f32,
    pub gs: f32,
    pub settings: TouchLayoutSettings,
}

/// A control a player can move, resize or fade. The order is the index into
/// [`TouchLayoutSettings::placements`], so new controls go at the end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchControl {
    Stick,
    Jump,
    Chat,
    Inventory,
    Pause,
}

impl TouchControl {
    /// Every control the layout editor shows, in placement order.
    pub const ALL: [Self; 5] = [
        Self::Stick,
        Self::Jump,
        Self::Chat,
        Self::Inventory,
        Self::Pause,
    ];

    /// The name the layout editor shows.
    pub fn name(self) -> &'static str {
        match self {
            Self::Stick => "Move",
            Self::Jump => "Jump",
            Self::Chat => "Chat",
            Self::Inventory => "Inventory",
            Self::Pause => "Pause",
        }
    }

    /// The movable button this control is, or `None` for the stick.
    pub fn button(self) -> Option<TouchButton> {
        match self {
            Self::Stick => None,
            Self::Jump => Some(TouchButton::Jump),
            Self::Chat => Some(TouchButton::Chat),
            Self::Inventory => Some(TouchButton::Inventory),
            Self::Pause => Some(TouchButton::Pause),
        }
    }
}

/// One control's saved placement. Every field is optional: an absent field
/// keeps the built-in position, size or opacity, so a save written before a
/// control existed still loads and a partial edit changes only what it names.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    /// The centre as a fraction of the screen, `(0, 0)` top-left, `(1, 1)`
    /// bottom-right. `None` keeps the built-in centre.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub centre: Option<(f32, f32)>,
    /// The control's size multiplier (1.0 is the built-in size).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f32>,
    /// The control's draw opacity, 0..1. `None` keeps the built-in opacity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f32>,
}

impl Placement {
    /// Whether this placement says anything; an empty one is dropped on save.
    pub fn is_default(&self) -> bool {
        self.centre.is_none() && self.size.is_none() && self.opacity.is_none()
    }

    /// The size multiplier, clamped to a sane range so a bad save cannot make
    /// a control unusable.
    pub fn size_or_default(&self) -> f32 {
        self.size.unwrap_or(1.0).clamp(SIZE_MIN, SIZE_MAX)
    }

    /// The draw opacity, clamped to `0..1`.
    pub fn opacity_or_default(&self) -> f32 {
        self.opacity.unwrap_or(1.0).clamp(0.0, 1.0)
    }
}

/// A player-added touch button: a label, the action it performs, and where it
/// sits. Unlike the built-in controls, any number of these can exist, so they
/// live in their own list rather than the fixed [`TouchControl`] placement
/// slots.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CustomButton {
    /// The label drawn on the button and edited in the layout editor.
    #[serde(default)]
    pub label: String,
    /// The action's stable id ([`Action::id`]), e.g. `key.attack`. An unknown
    /// id leaves the button inert rather than failing the whole layout.
    #[serde(default)]
    pub action: String,
    /// Where the button sits, the same optional overrides a built-in uses.
    #[serde(default)]
    pub placement: Placement,
}

/// The longest label a custom button may carry, matching the editor's field.
pub const CUSTOM_LABEL_MAX: usize = 16;

/// The actions the editor's picker cycles through, most useful first: a
/// player adding a button usually wants attack or use.
pub const CUSTOM_ACTIONS: &[Action] = &[
    Action::Destroy,
    Action::Use,
    Action::Jump,
    Action::Sneak,
    Action::Sprint,
    Action::ToggleInventory,
    Action::OpenChat,
    Action::OpenCommands,
    Action::OpenMenu,
    Action::ChangePerspective,
    Action::DropItem,
    Action::SwapOffhand,
    Action::PickBlock,
    Action::Screenshot,
    Action::ViewPlayerList,
    Action::ToggleGui,
    Action::Forward,
    Action::Backward,
    Action::StrafeLeft,
    Action::StrafeRight,
    Action::NextHotbarSlot,
    Action::PreviousHotbarSlot,
    Action::Hotbar1,
    Action::Hotbar2,
    Action::Hotbar3,
    Action::Hotbar4,
    Action::Hotbar5,
    Action::Hotbar6,
    Action::Hotbar7,
    Action::Hotbar8,
    Action::Hotbar9,
];

/// Whether a custom button bound to `action` stays down while pressed. Attack
/// and use do; a one-shot like the inventory does not, so its tap is emitted
/// once and released the same frame.
pub fn is_held_action(action: Action) -> bool {
    matches!(
        action,
        Action::Destroy
            | Action::Use
            | Action::Jump
            | Action::Sneak
            | Action::Sprint
            | Action::Forward
            | Action::Backward
            | Action::StrafeLeft
            | Action::StrafeRight
    )
}

/// The action after `current` in [`CUSTOM_ACTIONS`], wrapping. An unbound or
/// unknown current action starts the cycle at the first entry.
pub fn next_custom_action(current: Option<Action>) -> Action {
    let Some(current) = current else {
        return CUSTOM_ACTIONS[0];
    };
    match CUSTOM_ACTIONS.iter().position(|&a| a == current) {
        Some(index) => CUSTOM_ACTIONS[(index + 1) % CUSTOM_ACTIONS.len()],
        None => CUSTOM_ACTIONS[0],
    }
}

/// A control the layout editor can move, resize or fade: one of the built-in
/// [`TouchControl`]s, or a player-added button by index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditTarget {
    Builtin(TouchControl),
    Custom(usize),
}

/// The player's saved touch layout, as JSON. Missing fields take their
/// defaults, so later controls and options can be added without resetting a
/// saved file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TouchLayoutSettings {
    /// Per-control placement, indexed by [`TouchControl`]. Kept as a `Vec` so
    /// a shorter save (an older version) loads; a missing entry is default.
    #[serde(default)]
    pub placements: Vec<Placement>,
    /// A global size multiplier applied on top of each control's own size.
    #[serde(default = "one")]
    pub scale: f32,
    /// Player-added buttons, in creation order. Empty on the built-in layout.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub custom: Vec<CustomButton>,
}

/// The default is the built-in layout: no placements, scale 1.0. This cannot
/// be derived, because `f32::default()` is 0.0, which would zero every control.
impl Default for TouchLayoutSettings {
    fn default() -> Self {
        Self {
            placements: Vec::new(),
            scale: 1.0,
            custom: Vec::new(),
        }
    }
}

fn one() -> f32 {
    1.0
}

/// The smallest and largest a control's size multiplier may be.
pub const SIZE_MIN: f32 = 0.5;
pub const SIZE_MAX: f32 = 2.0;

impl TouchLayoutSettings {
    /// This control's placement, or the default when the save is short.
    pub fn placement(&self, control: TouchControl) -> Placement {
        self.placements
            .get(control as usize)
            .copied()
            .unwrap_or_default()
    }

    /// This control's placement for editing, extending the list with defaults
    /// so the returned entry is live.
    fn placement_mut(&mut self, control: TouchControl) -> &mut Placement {
        let index = control as usize;
        if self.placements.len() <= index {
            self.placements.resize(index + 1, Placement::default());
        }
        &mut self.placements[index]
    }

    /// Move a control's centre to a screen fraction, clamped to `0..1` so a bad
    /// drag cannot put it off screen.
    pub fn set_centre(&mut self, control: TouchControl, x: f32, y: f32) {
        let p = self.placement_mut(control);
        p.centre = Some((x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)));
    }

    /// Set a control's size multiplier, clamped to `SIZE_MIN..=SIZE_MAX`.
    pub fn set_size(&mut self, control: TouchControl, size: f32) {
        self.placement_mut(control).size = Some(size.clamp(SIZE_MIN, SIZE_MAX));
    }

    /// Set a control's draw opacity, clamped to `0..=1`.
    pub fn set_opacity(&mut self, control: TouchControl, opacity: f32) {
        self.placement_mut(control).opacity = Some(opacity.clamp(0.0, 1.0));
    }

    /// The global size multiplier, as set (the accessor [`Self::scale`]
    /// clamps).
    pub fn scale_value(&self) -> f32 {
        self.scale
    }

    /// Set the global size multiplier, clamped to `SIZE_MIN..=SIZE_MAX`.
    pub fn set_scale(&mut self, scale: f32) {
        self.scale = scale.clamp(SIZE_MIN, SIZE_MAX);
    }

    /// Restore every control and the global scale to the built-in layout.
    pub fn reset_all(&mut self) {
        self.placements.clear();
        self.scale = 1.0;
        self.custom.clear();
    }

    /// Add a button performing `action` with `label`, placed at the screen
    /// fraction `centre`. Returns the new button's index.
    pub fn add_custom(&mut self, label: &str, action: Action, centre: (f32, f32)) -> usize {
        let placement = Placement {
            centre: Some((centre.0.clamp(0.0, 1.0), centre.1.clamp(0.0, 1.0))),
            ..Placement::default()
        };
        self.custom.push(CustomButton {
            label: label.chars().take(CUSTOM_LABEL_MAX).collect(),
            action: action.id().as_str().to_owned(),
            placement,
        });
        self.custom.len() - 1
    }

    /// Remove the custom button at `index`, if it exists.
    pub fn remove_custom(&mut self, index: usize) {
        if index < self.custom.len() {
            self.custom.remove(index);
        }
    }

    /// Set a custom button's label, clamped to [`CUSTOM_LABEL_MAX`].
    pub fn set_custom_label(&mut self, index: usize, label: &str) {
        if let Some(button) = self.custom.get_mut(index) {
            button.label = label.chars().take(CUSTOM_LABEL_MAX).collect();
        }
    }

    /// Set a custom button's action, storing its stable id.
    pub fn set_custom_action(&mut self, index: usize, action: Action) {
        if let Some(button) = self.custom.get_mut(index) {
            button.action = action.id().as_str().to_owned();
        }
    }

    /// The action a custom button performs, or `None` for an unknown id.
    pub fn custom_action(&self, index: usize) -> Option<Action> {
        self.custom
            .get(index)
            .and_then(|button| Action::from_id(&button.action))
    }

    /// Move an edit target's centre to a screen fraction.
    pub fn set_target_centre(&mut self, target: EditTarget, x: f32, y: f32) {
        match target {
            EditTarget::Builtin(control) => self.set_centre(control, x, y),
            EditTarget::Custom(index) => {
                if let Some(button) = self.custom.get_mut(index) {
                    button.placement.centre = Some((x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)));
                }
            }
        }
    }

    /// Set an edit target's size multiplier.
    pub fn set_target_size(&mut self, target: EditTarget, size: f32) {
        match target {
            EditTarget::Builtin(control) => self.set_size(control, size),
            EditTarget::Custom(index) => {
                if let Some(button) = self.custom.get_mut(index) {
                    button.placement.size = Some(size.clamp(SIZE_MIN, SIZE_MAX));
                }
            }
        }
    }

    /// Set an edit target's draw opacity.
    pub fn set_target_opacity(&mut self, target: EditTarget, opacity: f32) {
        match target {
            EditTarget::Builtin(control) => self.set_opacity(control, opacity),
            EditTarget::Custom(index) => {
                if let Some(button) = self.custom.get_mut(index) {
                    button.placement.opacity = Some(opacity.clamp(0.0, 1.0));
                }
            }
        }
    }

    /// An edit target's placement, for the editor's sliders.
    pub fn target_placement(&self, target: EditTarget) -> Placement {
        match target {
            EditTarget::Builtin(control) => self.placement(control),
            EditTarget::Custom(index) => self
                .custom
                .get(index)
                .map_or_else(Default::default, |button| button.placement),
        }
    }

    /// The global scale, clamped so a bad save cannot zero the controls out.
    pub fn scale(&self) -> f32 {
        self.scale.clamp(SIZE_MIN, SIZE_MAX)
    }

    /// Load the saved layout, or the default when the file is missing or
    /// unreadable. A corrupt file is not an error: it falls back to default.
    pub fn load(path: &std::path::Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Write the layout, dropping trailing empty placements so the file stays
    /// small. Atomic, like the other options writes.
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        let mut trimmed = self.clone();
        while trimmed.placements.last().is_some_and(Placement::is_default) {
            trimmed.placements.pop();
        }
        let json = serde_json::to_string_pretty(&trimmed).map_err(std::io::Error::other)?;
        crate::util::write_atomic(path, json.as_bytes())
    }
}

/// A movable button. The order is the index into [`TouchLayoutSettings`]'s
/// placements, so it matches [`TouchControl`] one-to-one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchButton {
    Jump,
    Chat,
    Inventory,
    Pause,
}

impl TouchButton {
    pub const ALL: [Self; 4] = [Self::Jump, Self::Chat, Self::Inventory, Self::Pause];

    /// The movable control this button edits and reads its placement from.
    pub fn control(self) -> TouchControl {
        match self {
            Self::Jump => TouchControl::Jump,
            Self::Chat => TouchControl::Chat,
            Self::Inventory => TouchControl::Inventory,
            Self::Pause => TouchControl::Pause,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Jump => "Jump",
            Self::Chat => "Chat",
            Self::Inventory => "Inv",
            Self::Pause => "| |",
        }
    }

    /// The action a tap presses; Jump is held instead.
    fn tapped_action(self) -> Option<Action> {
        match self {
            Self::Jump => None,
            Self::Chat => Some(Action::OpenChat),
            Self::Inventory => Some(Action::ToggleInventory),
            Self::Pause => Some(Action::OpenMenu),
        }
    }
}

impl TouchLayout {
    /// The built-in centre for a button, before any saved placement. This is
    /// the geometry the fixed-layout controls always had.
    pub fn default_button_centre(&self, button: TouchButton) -> Vec2 {
        let inset = 2.0 * BUTTON_RADIUS * self.gs;
        match button {
            TouchButton::Jump => Vec2::new(self.width - inset, self.height - 1.5 * inset),
            TouchButton::Chat => Vec2::new(self.width - 4.0 * inset, inset),
            TouchButton::Inventory => Vec2::new(self.width - 2.5 * inset, inset),
            TouchButton::Pause => Vec2::new(self.width - inset, inset),
        }
    }

    /// A button's centre, honouring a saved placement. A placement's centre is
    /// a screen fraction; a saved one is clamped so the button stays on screen.
    pub fn button_centre(&self, button: TouchButton) -> Vec2 {
        let placement = self.settings.placement(button.control());
        match placement.centre {
            Some((x, y)) => {
                let radius = self.button_radius(button);
                Vec2::new(
                    (x * self.width).clamp(radius, self.width - radius),
                    (y * self.height).clamp(radius, self.height - radius),
                )
            }
            None => self.default_button_centre(button),
        }
    }

    /// A button's radius in pixels, with the saved size applied.
    pub fn button_radius(&self, button: TouchButton) -> f32 {
        BUTTON_RADIUS * self.gs * self.button_scale(button)
    }

    fn button_scale(&self, button: TouchButton) -> f32 {
        self.settings.scale() * self.settings.placement(button.control()).size_or_default()
    }

    /// A button's draw opacity, honouring a saved placement.
    pub fn button_opacity(&self, button: TouchButton) -> f32 {
        self.settings
            .placement(button.control())
            .opacity_or_default()
    }

    /// Where the stick rests until a press places it, honouring a saved
    /// placement.
    pub fn stick_home(&self) -> Vec2 {
        let placement = self.settings.placement(TouchControl::Stick);
        match placement.centre {
            Some((x, y)) => {
                let radius = self.stick_radius();
                Vec2::new(
                    (x * self.width).clamp(radius, self.width - radius),
                    (y * self.height).clamp(radius, self.height - radius),
                )
            }
            None => {
                let inset = 1.6 * STICK_RADIUS * self.gs;
                Vec2::new(inset, self.height - inset)
            }
        }
    }

    /// The stick's travel radius in pixels, with the saved size applied.
    pub fn stick_radius(&self) -> f32 {
        let placement = self.settings.placement(TouchControl::Stick);
        STICK_RADIUS * self.gs * self.settings.scale() * placement.size_or_default()
    }

    /// The stick's draw opacity, honouring a saved placement.
    pub fn stick_opacity(&self) -> f32 {
        self.settings
            .placement(TouchControl::Stick)
            .opacity_or_default()
    }

    fn button_at(&self, p: Vec2) -> Option<TouchButton> {
        TouchButton::ALL
            .into_iter()
            .find(|&b| p.distance(self.button_centre(b)) <= self.button_radius(b))
    }

    /// A control's centre in screen pixels, whichever kind it is.
    pub fn control_centre(&self, control: TouchControl) -> Vec2 {
        match control.button() {
            Some(button) => self.button_centre(button),
            None => self.stick_home(),
        }
    }

    /// A control's radius in pixels, whichever kind it is.
    pub fn control_radius(&self, control: TouchControl) -> f32 {
        match control.button() {
            Some(button) => self.button_radius(button),
            None => self.stick_radius(),
        }
    }

    /// The close button's centre over an in-game screen: the pause button's
    /// home, so the same corner of the screen both opens and closes the
    /// screens a touch player can reach. Drawn and hit-tested by the overlay
    /// only while a screen is open.
    pub fn close_centre(&self) -> Vec2 {
        self.button_centre(TouchButton::Pause)
    }

    /// The close button's radius in pixels, a little larger than a normal
    /// button for an easy target.
    pub fn close_radius(&self) -> f32 {
        CLOSE_RADIUS * self.gs * self.settings.scale()
    }

    /// Whether `p` is on the close button, within `slop` extra pixels.
    pub fn close_at(&self, p: Vec2, slop: f32) -> bool {
        p.distance(self.close_centre()) <= self.close_radius() + slop
    }

    /// The control under a point, for the layout editor: a button takes
    /// precedence over the stick, as in gameplay, and a small tolerance makes a
    /// control easy to grab.
    pub fn control_at(&self, p: Vec2) -> Option<TouchControl> {
        TouchControl::ALL.into_iter().find(|&control| {
            p.distance(self.control_centre(control))
                <= self.control_radius(control) + EDIT_GRAB_SLOP
        })
    }

    /// A custom button's centre in pixels, honouring its saved placement.
    pub fn custom_centre(&self, index: usize) -> Vec2 {
        let button = &self.settings.custom[index];
        let radius = self.custom_radius(index);
        match button.placement.centre {
            Some((x, y)) => Vec2::new(
                (x * self.width).clamp(radius, self.width - radius),
                (y * self.height).clamp(radius, self.height - radius),
            ),
            None => Vec2::new(self.width / 2.0, self.height / 2.0),
        }
    }

    /// A custom button's radius in pixels, with its saved size applied.
    pub fn custom_radius(&self, index: usize) -> f32 {
        let size = self.settings.custom[index].placement.size_or_default();
        BUTTON_RADIUS * self.gs * self.settings.scale() * size
    }

    /// A custom button's draw opacity, honouring its saved placement.
    pub fn custom_opacity(&self, index: usize) -> f32 {
        self.settings.custom[index].placement.opacity_or_default()
    }

    /// The custom button under a point, for gameplay input. Searched after the
    /// built-in buttons, so a custom button never steals a built-in's tap.
    fn custom_button_at(&self, p: Vec2) -> Option<usize> {
        (0..self.settings.custom.len())
            .find(|&i| p.distance(self.custom_centre(i)) <= self.custom_radius(i))
    }

    /// The editable control under a point, for the layout editor: custom
    /// buttons take precedence (so a new button is always grabbable), then the
    /// built-in controls, each with a small grab tolerance.
    pub fn edit_target_at(&self, p: Vec2) -> Option<EditTarget> {
        if let Some(index) = (0..self.settings.custom.len())
            .find(|&i| p.distance(self.custom_centre(i)) <= self.custom_radius(i) + EDIT_GRAB_SLOP)
        {
            return Some(EditTarget::Custom(index));
        }
        self.control_at(p).map(EditTarget::Builtin)
    }

    /// An edit target's centre in pixels.
    pub fn target_centre(&self, target: EditTarget) -> Vec2 {
        match target {
            EditTarget::Builtin(control) => self.control_centre(control),
            EditTarget::Custom(index) => self.custom_centre(index),
        }
    }

    /// An edit target's radius in pixels.
    pub fn target_radius(&self, target: EditTarget) -> f32 {
        match target {
            EditTarget::Builtin(control) => self.control_radius(control),
            EditTarget::Custom(index) => self.custom_radius(index),
        }
    }

    fn hotbar_slot_at(&self, p: Vec2) -> Option<u8> {
        crate::ui::hud::hotbar_slot_at(self.width, self.height, self.gs, (p.x, p.y))
    }
}

/// What a touch did, for `InputState` to apply as mouse and key input.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TouchOutput {
    /// Mouse counts to turn by.
    Look(Vec2),
    DestroyStart,
    DestroyEnd,
    /// A destroy that outlived gameplay: stop it without a release.
    DestroyCancel,
    Use,
    /// A tap of an action's key.
    Tap(Action),
    /// A custom button bound to a held action went down; it stays down until
    /// [`TouchOutput::CustomRelease`].
    CustomPress(Action),
    /// The held custom button above lifted, releasing its action.
    CustomRelease(Action),
    Hotbar(u8),
    PointerMove(Vec2),
    PointerDown,
    PointerUp,
}

enum Role {
    Stick {
        origin: Vec2,
        current: Vec2,
        /// [`STICK_RADIUS`] in pixels.
        radius: f32,
    },
    Look {
        start: Vec2,
        last: Vec2,
        since: Instant,
        still: bool,
        destroying: bool,
    },
    Jump,
    Pointer,
    /// A custom button bound to a held action, held until it lifts.
    CustomHeld(Action),
    /// A tap already handled on press; ignored until it lifts.
    Spent,
}

/// What the rest of the app looks like to a touch, so the overlay can decide
/// whether the touch is world input, screen input, or the close button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TouchContext {
    /// The world has the input (no screen, chat or pause menu open).
    pub gameplay: bool,
    /// A screen the touch close button can dismiss is up.
    pub close: bool,
}

#[derive(Default)]
pub struct TouchControls {
    touches: HashMap<u64, Role>,
    /// Any touch has arrived, so the overlay shows; only phones feed them.
    seen: bool,
}

impl TouchControls {
    pub fn seen(&self) -> bool {
        self.seen
    }

    /// Feeds one touch event. `ctx` says whether the world or a dismissable
    /// screen has the input; a touch keeps the role it started with until it
    /// lifts.
    pub fn on_touch(
        &mut self,
        id: u64,
        phase: TouchPhase,
        p: Vec2,
        ctx: TouchContext,
        layout: &TouchLayout,
        now: Instant,
    ) -> Vec<TouchOutput> {
        self.seen = true;
        match phase {
            TouchPhase::Started => self.start(id, p, ctx, layout, now),
            TouchPhase::Moved => self.drag(id, p, layout),
            TouchPhase::Ended => self.end(id, now, true, ctx.gameplay),
            TouchPhase::Cancelled => self.end(id, now, false, ctx.gameplay),
        }
    }

    fn start(
        &mut self,
        id: u64,
        p: Vec2,
        ctx: TouchContext,
        layout: &TouchLayout,
        now: Instant,
    ) -> Vec<TouchOutput> {
        let (role, out) = if !ctx.gameplay {
            // A screen the touch close button can dismiss: a tap there is
            // Escape, not a click behind the screen. The button is drawn only
            // when this flag is set, so the overlay and the hit test agree.
            if ctx.close && layout.close_at(p, EDIT_GRAB_SLOP) {
                (Role::Spent, vec![TouchOutput::Tap(Action::OpenMenu)])
            } else {
                (
                    Role::Pointer,
                    vec![TouchOutput::PointerMove(p), TouchOutput::PointerDown],
                )
            }
        } else if let Some(button) = layout.button_at(p) {
            match button.tapped_action() {
                Some(action) => (Role::Spent, vec![TouchOutput::Tap(action)]),
                None => (Role::Jump, Vec::new()),
            }
        } else if let Some(index) = layout.custom_button_at(p) {
            match layout.settings.custom_action(index) {
                Some(action) if is_held_action(action) => (
                    Role::CustomHeld(action),
                    vec![TouchOutput::CustomPress(action)],
                ),
                Some(action) => (Role::Spent, vec![TouchOutput::Tap(action)]),
                None => (Role::Spent, Vec::new()),
            }
        } else if let Some(slot) = layout.hotbar_slot_at(p) {
            (Role::Spent, vec![TouchOutput::Hotbar(slot)])
        } else if p.x < layout.width * STICK_ZONE {
            (
                Role::Stick {
                    origin: p,
                    current: p,
                    radius: layout.stick_radius(),
                },
                Vec::new(),
            )
        } else {
            (
                Role::Look {
                    start: p,
                    last: p,
                    since: now,
                    still: true,
                    destroying: false,
                },
                Vec::new(),
            )
        };
        self.touches.insert(id, role);
        out
    }

    fn drag(&mut self, id: u64, p: Vec2, layout: &TouchLayout) -> Vec<TouchOutput> {
        match self.touches.get_mut(&id) {
            Some(Role::Stick { current, .. }) => {
                *current = p;
                Vec::new()
            }
            Some(Role::Look {
                start, last, still, ..
            }) => {
                let delta = (p - *last) / layout.gs * LOOK_SCALE;
                *last = p;
                if p.distance(*start) > STILL_SLOP * layout.gs {
                    *still = false;
                }
                vec![TouchOutput::Look(delta)]
            }
            Some(Role::Pointer) => vec![TouchOutput::PointerMove(p)],
            _ => Vec::new(),
        }
    }

    /// A look touch lifted after a screen opened stays out of that screen.
    fn end(&mut self, id: u64, now: Instant, lifted: bool, gameplay: bool) -> Vec<TouchOutput> {
        match self.touches.remove(&id) {
            Some(Role::Look {
                destroying: true, ..
            }) => vec![if gameplay {
                TouchOutput::DestroyEnd
            } else {
                TouchOutput::DestroyCancel
            }],
            Some(Role::Look { since, still, .. })
                if gameplay && lifted && still && now.duration_since(since) < TAP_TIME =>
            {
                vec![TouchOutput::Use]
            }
            Some(Role::Pointer) => vec![TouchOutput::PointerUp],
            Some(Role::CustomHeld(action)) => vec![TouchOutput::CustomRelease(action)],
            _ => Vec::new(),
        }
    }

    /// Once a frame: a look press held still long enough starts destroying,
    /// while the world has the input.
    pub fn tick(&mut self, now: Instant, gameplay: bool) -> Vec<TouchOutput> {
        let mut out = Vec::new();
        if !gameplay {
            return out;
        }
        for role in self.touches.values_mut() {
            if let Role::Look {
                since,
                still: true,
                destroying,
                ..
            } = role
                && !*destroying
                && now.duration_since(*since) >= HOLD_TIME
            {
                *destroying = true;
                out.push(TouchOutput::DestroyStart);
            }
        }
        out
    }

    /// The stick's deflection, unit length at most, `+y` up like a gamepad
    /// stick.
    pub fn stick(&self) -> Option<Vec2> {
        self.touches.values().find_map(|role| match role {
            Role::Stick {
                origin,
                current,
                radius,
            } => {
                let v = (*current - *origin) / *radius;
                Some(Vec2::new(v.x, -v.y).clamp_length_max(1.0))
            }
            _ => None,
        })
    }

    /// Where a held stick was placed, for drawing.
    pub fn stick_origin(&self) -> Option<Vec2> {
        self.touches.values().find_map(|role| match role {
            Role::Stick { origin, .. } => Some(*origin),
            _ => None,
        })
    }

    pub fn jump_held(&self) -> bool {
        self.touches.values().any(|role| matches!(role, Role::Jump))
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use glam::Vec2;
    use winit::event::TouchPhase;

    use super::{
        Action, BUTTON_RADIUS, CUSTOM_ACTIONS, CUSTOM_LABEL_MAX, CustomButton, EditTarget,
        Placement, SIZE_MAX, SIZE_MIN, TouchButton, TouchContext, TouchControl, TouchControls,
        TouchLayout, TouchLayoutSettings, TouchOutput, next_custom_action,
    };

    /// The built-in layout, which every behavioural test uses.
    fn layout() -> TouchLayout {
        TouchLayout {
            width: 1600.0,
            height: 900.0,
            gs: 2.0,
            settings: TouchLayoutSettings::default(),
        }
    }

    /// Right of the stick zone, above the hotbar, away from the buttons.
    const LOOK: Vec2 = Vec2::new(1000.0, 400.0);
    const STICK: Vec2 = Vec2::new(200.0, 600.0);

    fn ms(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    fn ctx(gameplay: bool, close: bool) -> TouchContext {
        TouchContext { gameplay, close }
    }

    fn touch(
        c: &mut TouchControls,
        id: u64,
        phase: TouchPhase,
        p: Vec2,
        at: Instant,
    ) -> Vec<TouchOutput> {
        c.on_touch(id, phase, p, ctx(true, false), &layout(), at)
    }

    #[test]
    fn a_quick_still_tap_uses() {
        let (mut c, t) = (TouchControls::default(), Instant::now());
        assert!(touch(&mut c, 1, TouchPhase::Started, LOOK, t).is_empty());
        assert!(c.tick(ms(t, 100), true).is_empty());
        assert_eq!(
            touch(&mut c, 1, TouchPhase::Ended, LOOK, ms(t, 150)),
            [TouchOutput::Use]
        );
        assert!(c.seen());
    }

    #[test]
    fn a_look_touch_stays_out_of_a_screen_opened_under_it() {
        let (mut c, t) = (TouchControls::default(), Instant::now());
        touch(&mut c, 1, TouchPhase::Started, LOOK, t);
        assert!(c.tick(ms(t, 300), false).is_empty());
        assert!(
            c.on_touch(
                1,
                TouchPhase::Ended,
                LOOK,
                ctx(false, false),
                &layout(),
                ms(t, 350)
            )
            .is_empty()
        );

        touch(&mut c, 2, TouchPhase::Started, LOOK, t);
        assert_eq!(c.tick(ms(t, 300), true), [TouchOutput::DestroyStart]);
        assert_eq!(
            c.on_touch(
                2,
                TouchPhase::Ended,
                LOOK,
                ctx(false, false),
                &layout(),
                ms(t, 400)
            ),
            [TouchOutput::DestroyCancel]
        );
    }

    #[test]
    fn a_still_hold_destroys_until_lifted() {
        let (mut c, t) = (TouchControls::default(), Instant::now());
        touch(&mut c, 1, TouchPhase::Started, LOOK, t);
        assert_eq!(c.tick(ms(t, 300), true), [TouchOutput::DestroyStart]);
        assert!(c.tick(ms(t, 400), true).is_empty());
        assert_eq!(
            touch(&mut c, 1, TouchPhase::Ended, LOOK, ms(t, 500)),
            [TouchOutput::DestroyEnd]
        );
    }

    #[test]
    fn a_drag_looks_and_neither_uses_nor_destroys() {
        let (mut c, t) = (TouchControls::default(), Instant::now());
        touch(&mut c, 1, TouchPhase::Started, LOOK, t);
        let moved = LOOK + Vec2::new(40.0, -20.0);
        assert_eq!(
            touch(&mut c, 1, TouchPhase::Moved, moved, ms(t, 50)),
            [TouchOutput::Look(Vec2::new(50.0, -25.0))]
        );
        assert!(c.tick(ms(t, 400), true).is_empty());
        assert!(touch(&mut c, 1, TouchPhase::Ended, moved, ms(t, 100)).is_empty());
    }

    #[test]
    fn the_stick_clamps_to_unit_length_with_y_up() {
        let (mut c, t) = (TouchControls::default(), Instant::now());
        touch(&mut c, 2, TouchPhase::Started, STICK, t);
        touch(
            &mut c,
            2,
            TouchPhase::Moved,
            STICK + Vec2::new(0.0, -30.0),
            t,
        );
        assert_eq!(c.stick(), Some(Vec2::new(0.0, 0.5)));
        touch(
            &mut c,
            2,
            TouchPhase::Moved,
            STICK + Vec2::new(500.0, 0.0),
            t,
        );
        assert_eq!(c.stick(), Some(Vec2::new(1.0, 0.0)));
        touch(&mut c, 2, TouchPhase::Ended, STICK, t);
        assert_eq!(c.stick(), None);
    }

    #[test]
    fn move_and_look_at_once() {
        let (mut c, t) = (TouchControls::default(), Instant::now());
        touch(&mut c, 1, TouchPhase::Started, STICK, t);
        touch(&mut c, 2, TouchPhase::Started, LOOK, t);
        touch(
            &mut c,
            1,
            TouchPhase::Moved,
            STICK + Vec2::new(60.0, 0.0),
            t,
        );
        let look = touch(&mut c, 2, TouchPhase::Moved, LOOK + Vec2::new(8.0, 0.0), t);
        assert_eq!(look, [TouchOutput::Look(Vec2::new(10.0, 0.0))]);
        assert_eq!(c.stick(), Some(Vec2::new(1.0, 0.0)));
    }

    #[test]
    fn buttons_and_hotbar_fire_on_press() {
        let (mut c, t) = (TouchControls::default(), Instant::now());
        let pause = layout().button_centre(TouchButton::Pause);
        assert_eq!(
            touch(&mut c, 1, TouchPhase::Started, pause, t),
            [TouchOutput::Tap(Action::OpenMenu)]
        );
        assert!(touch(&mut c, 1, TouchPhase::Ended, pause, t).is_empty());
        assert_eq!(
            touch(
                &mut c,
                4,
                TouchPhase::Started,
                layout().button_centre(TouchButton::Chat),
                t
            ),
            [TouchOutput::Tap(Action::OpenChat)]
        );

        touch(
            &mut c,
            2,
            TouchPhase::Started,
            layout().button_centre(TouchButton::Jump),
            t,
        );
        assert!(c.jump_held());
        touch(&mut c, 2, TouchPhase::Ended, Vec2::ZERO, t);
        assert!(!c.jump_held());

        // The hotbar's first slot starts 1 unit in from its left edge.
        let first_slot = Vec2::new(
            layout().width / 2.0 - 91.0 * 2.0 + 10.0,
            layout().height - 10.0,
        );
        assert_eq!(
            touch(&mut c, 3, TouchPhase::Started, first_slot, t),
            [TouchOutput::Hotbar(0)]
        );
    }

    #[test]
    fn outside_gameplay_a_touch_is_the_pointer() {
        let (mut c, t) = (TouchControls::default(), Instant::now());
        let p = Vec2::new(300.0, 200.0);
        assert_eq!(
            c.on_touch(1, TouchPhase::Started, p, ctx(false, false), &layout(), t),
            [TouchOutput::PointerMove(p), TouchOutput::PointerDown]
        );
        assert_eq!(
            c.on_touch(1, TouchPhase::Ended, p, ctx(false, false), &layout(), t),
            [TouchOutput::PointerUp]
        );
    }

    #[test]
    fn the_close_button_dismisses_a_screen_but_not_the_pointer_elsewhere() {
        let l = layout();
        let close = l.close_centre();
        // A tap on the close button is Escape, not a click behind the screen.
        let mut c = TouchControls::default();
        let t = Instant::now();
        assert_eq!(
            c.on_touch(1, TouchPhase::Started, close, ctx(false, true), &l, t),
            [TouchOutput::Tap(Action::OpenMenu)]
        );
        assert!(
            c.on_touch(1, TouchPhase::Ended, close, ctx(false, true), &l, t)
                .is_empty()
        );

        // A tap elsewhere is still the pointer.
        let mut c2 = TouchControls::default();
        let elsewhere = Vec2::new(400.0, 400.0);
        assert_eq!(
            c2.on_touch(1, TouchPhase::Started, elsewhere, ctx(false, true), &l, t),
            [
                TouchOutput::PointerMove(elsewhere),
                TouchOutput::PointerDown
            ]
        );
    }

    #[test]
    fn the_close_button_is_inert_when_no_screen_is_up() {
        // `close` is false, so the same point falls through to the pause
        // button (which rests there) while the world has the pointer.
        let l = layout();
        let close = l.close_centre();
        let mut c = TouchControls::default();
        let t = Instant::now();
        assert_eq!(
            c.on_touch(1, TouchPhase::Started, close, ctx(true, false), &l, t),
            [TouchOutput::Tap(Action::OpenMenu)]
        );
    }

    #[test]
    fn the_close_geometry_follows_a_saved_pause_placement() {
        let mut l = layout();
        // Pause is the last control, so the placement list runs Stick..Pause.
        l.settings.placements = vec![
            Placement::default(),
            Placement::default(),
            Placement::default(),
            Placement::default(),
            Placement {
                centre: Some((0.1, 0.9)),
                ..Placement::default()
            },
        ];
        assert_eq!(l.close_centre(), Vec2::new(0.1 * 1600.0, 0.9 * 900.0));
        assert!(l.close_at(l.close_centre(), 0.0));
        assert!(!l.close_at(Vec2::ZERO, 0.0));
    }

    #[test]
    fn default_layout_matches_the_builtin_geometry() {
        let l = layout();
        // An absent placement must not move anything.
        assert_eq!(
            l.button_centre(TouchButton::Jump),
            l.default_button_centre(TouchButton::Jump)
        );
        for b in TouchButton::ALL {
            assert_eq!(l.button_centre(b), l.default_button_centre(b));
            assert_eq!(l.button_opacity(b), 1.0);
        }
        assert_eq!(
            l.stick_home(),
            Vec2::new(1.6 * 30.0 * 2.0, 900.0 - 1.6 * 30.0 * 2.0)
        );
    }

    #[test]
    fn a_saved_centre_moves_the_button_and_the_hit_test() {
        let mut l = layout();
        l.settings.placements = vec![
            Placement::default(),
            Placement {
                centre: Some((0.25, 0.75)),
                ..Placement::default()
            },
        ];
        let jump = l.button_centre(TouchButton::Jump);
        assert_eq!(jump, Vec2::new(0.25 * 1600.0, 0.75 * 900.0));

        // A tap at the new centre is the jump; the built-in centre is not.
        let mut c = TouchControls::default();
        let t = Instant::now();
        c.on_touch(1, TouchPhase::Started, jump, ctx(true, false), &l, t);
        assert!(c.jump_held());
        assert!(
            c.on_touch(1, TouchPhase::Ended, jump, ctx(true, false), &l, t)
                .is_empty()
        );

        let old = l.default_button_centre(TouchButton::Jump);
        let mut c2 = TouchControls::default();
        let out = c2.on_touch(1, TouchPhase::Started, old, ctx(true, false), &l, t);
        assert!(out.is_empty(), "the built-in centre is no longer the jump");
        assert!(!c2.jump_held());
    }

    #[test]
    fn a_saved_size_scales_the_button_radius() {
        let mut l = layout();
        l.settings.placements = vec![
            Placement::default(),
            Placement {
                size: Some(1.5),
                ..Placement::default()
            },
        ];
        assert_eq!(l.button_radius(TouchButton::Jump), 14.0 * 2.0 * 1.5);
        // The global scale compounds the per-control size.
        l.settings.scale = 0.5;
        assert_eq!(l.button_radius(TouchButton::Jump), 14.0 * 2.0 * 1.5 * 0.5);
    }

    #[test]
    fn opacity_follows_the_placement_and_clamps() {
        let mut l = layout();
        l.settings.placements = vec![
            Placement {
                opacity: Some(0.4),
                ..Placement::default()
            },
            Placement {
                opacity: Some(5.0),
                ..Placement::default()
            },
        ];
        assert_eq!(l.stick_opacity(), 0.4);
        assert_eq!(l.button_opacity(TouchButton::Jump), 1.0);
        assert_eq!(l.button_opacity(TouchButton::Chat), 1.0);
    }

    #[test]
    fn a_corrupt_or_short_save_still_loads() {
        let dir = std::env::temp_dir().join(format!("pomme-touch-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("touch_layout.json");

        // Missing file: the default layout.
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            TouchLayoutSettings::load(&path),
            TouchLayoutSettings::default()
        );

        // Corrupt file: still the default, not a panic.
        std::fs::write(&path, b"{ this is not json").unwrap();
        assert_eq!(
            TouchLayoutSettings::load(&path),
            TouchLayoutSettings::default()
        );

        // A short save with only the stick placed loads, and the rest default.
        std::fs::write(&path, br#"{"placements":[{"centre":[0.3,0.8]}]}"#).unwrap();
        let loaded = TouchLayoutSettings::load(&path);
        assert_eq!(
            loaded.placement(TouchControl::Stick).centre,
            Some((0.3, 0.8))
        );
        assert_eq!(loaded.placement(TouchControl::Pause), Placement::default());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_round_trips_and_drops_trailing_defaults() {
        let dir = std::env::temp_dir().join(format!("pomme-touch-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("touch_layout.json");

        let settings = TouchLayoutSettings {
            placements: vec![
                Placement {
                    centre: Some((0.2, 0.7)),
                    size: Some(1.25),
                    opacity: Some(0.6),
                },
                Placement::default(),
                Placement::default(),
            ],
            scale: 1.1,
            custom: Vec::new(),
        };
        settings.save(&path).unwrap();

        // The empty trailing placements are gone from the file.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("placements\": [\n    {},\n    {}"), "{raw}");

        let loaded = TouchLayoutSettings::load(&path);
        assert_eq!(loaded.placements.len(), 1);
        assert_eq!(loaded.scale, 1.1);
        assert_eq!(
            loaded.placement(TouchControl::Stick),
            Placement {
                centre: Some((0.2, 0.7)),
                size: Some(1.25),
                opacity: Some(0.6),
            }
        );

        // Saving the loaded settings again is stable.
        loaded.save(&path).unwrap();
        assert_eq!(TouchLayoutSettings::load(&path), loaded);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_saved_centre_is_clamped_onto_the_screen() {
        let mut l = layout();
        l.settings.placements = vec![Placement {
            centre: Some((-1.0, 5.0)),
            ..Placement::default()
        }];
        let home = l.stick_home();
        let r = l.stick_radius();
        assert_eq!(home.x, r);
        assert_eq!(home.y, 900.0 - r);
    }

    #[test]
    fn size_and_scale_are_clamped() {
        let mut l = layout();
        l.settings.scale = 0.0;
        l.settings.placements = vec![Placement {
            size: Some(100.0),
            ..Placement::default()
        }];
        // 100.0 clamps to SIZE_MAX, 0.0 scale clamps to SIZE_MIN.
        assert_eq!(l.stick_radius(), 30.0 * 2.0 * 2.0 * 0.5);
    }

    #[test]
    fn the_edit_api_sets_clamps_and_resets() {
        let mut s = TouchLayoutSettings::default();

        // A set grows the list and lands the control where it was put.
        s.set_centre(TouchControl::Jump, 0.25, 0.75);
        assert_eq!(s.placement(TouchControl::Jump).centre, Some((0.25, 0.75)));
        // A set centre is clamped to the screen.
        s.set_centre(TouchControl::Jump, -1.0, 2.0);
        assert_eq!(s.placement(TouchControl::Jump).centre, Some((0.0, 1.0)));

        // Size and opacity clamp to their ranges.
        s.set_size(TouchControl::Stick, 9.0);
        assert_eq!(s.placement(TouchControl::Stick).size, Some(SIZE_MAX));
        s.set_opacity(TouchControl::Stick, -0.5);
        assert_eq!(s.placement(TouchControl::Stick).opacity, Some(0.0));
        s.set_scale(0.0);
        assert_eq!(s.scale_value(), SIZE_MIN);

        // Reset one control leaves the others; reset all clears everything.
        s.reset_all();
        assert_eq!(s, TouchLayoutSettings::default());
    }

    #[test]
    fn an_edit_survives_a_save_and_load() {
        let dir = std::env::temp_dir().join(format!("pomme-touch-edit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("touch_layout.json");

        let mut s = TouchLayoutSettings::default();
        s.set_centre(TouchControl::Inventory, 0.4, 0.1);
        s.set_size(TouchControl::Inventory, 1.4);
        s.set_opacity(TouchControl::Inventory, 0.7);
        s.set_scale(1.2);
        s.save(&path).unwrap();

        assert_eq!(TouchLayoutSettings::load(&path), s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_custom_button_adds_names_binds_and_removes() {
        let mut s = TouchLayoutSettings::default();
        let index = s.add_custom("Attack", Action::Destroy, (0.5, 0.5));
        assert_eq!(index, 0);
        assert_eq!(s.custom.len(), 1);
        assert_eq!(s.custom_action(0), Some(Action::Destroy));
        assert_eq!(s.custom[0].placement.centre, Some((0.5, 0.5)));

        // The label clamps to the field's maximum, and the action round-trips.
        s.set_custom_label(0, &"x".repeat(CUSTOM_LABEL_MAX + 8));
        assert_eq!(s.custom[0].label.chars().count(), CUSTOM_LABEL_MAX);
        s.set_custom_action(0, Action::Use);
        assert_eq!(s.custom_action(0), Some(Action::Use));

        // Removing drops it, and the id survives a save/load round trip.
        let dir = std::env::temp_dir().join(format!("pomme-touch-custom-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("touch_layout.json");
        s.save(&path).unwrap();
        let loaded = TouchLayoutSettings::load(&path);
        assert_eq!(loaded.custom.len(), 1);
        assert_eq!(loaded.custom_action(0), Some(Action::Use));

        s.remove_custom(0);
        assert!(s.custom.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unknown_custom_action_id_is_inert() {
        let mut s = TouchLayoutSettings::default();
        s.custom.push(CustomButton {
            label: "Broken".into(),
            action: "key.does.not.exist".into(),
            placement: Placement::default(),
        });
        assert_eq!(s.custom_action(0), None);
    }

    #[test]
    fn the_action_picker_cycles_and_wraps() {
        // An unbound button starts the cycle at the first entry.
        assert_eq!(next_custom_action(None), CUSTOM_ACTIONS[0]);
        // A known action steps forward, and the last wraps to the first.
        assert_eq!(next_custom_action(Some(Action::Destroy)), CUSTOM_ACTIONS[1]);
        let last = *CUSTOM_ACTIONS.last().unwrap();
        assert_eq!(next_custom_action(Some(last)), CUSTOM_ACTIONS[0]);
    }

    #[test]
    fn a_custom_button_fires_its_action() {
        // Attack is a held action: it stays down until the finger lifts.
        let mut settings = TouchLayoutSettings::default();
        let index = settings.add_custom("Attack", Action::Destroy, (0.5, 0.5));
        let mut l = layout();
        l.settings = settings.clone();
        let at = l.custom_centre(index);

        let mut c = TouchControls::default();
        let t = Instant::now();
        assert_eq!(
            c.on_touch(1, TouchPhase::Started, at, ctx(true, false), &l, t),
            [TouchOutput::CustomPress(Action::Destroy)]
        );
        assert_eq!(
            c.on_touch(1, TouchPhase::Ended, at, ctx(true, false), &l, t),
            [TouchOutput::CustomRelease(Action::Destroy)]
        );

        // The inventory is a one-shot: a tap fires once and releases itself.
        let index = settings.add_custom("Bag", Action::ToggleInventory, (0.2, 0.2));
        let mut l = layout();
        l.settings = settings.clone();
        let at = l.custom_centre(index);
        assert_eq!(
            c.on_touch(2, TouchPhase::Started, at, ctx(true, false), &l, t),
            [TouchOutput::Tap(Action::ToggleInventory)]
        );
        assert!(
            c.on_touch(2, TouchPhase::Ended, at, ctx(true, false), &l, t)
                .is_empty()
        );
    }

    #[test]
    fn a_custom_button_is_an_edit_target() {
        let mut settings = TouchLayoutSettings::default();
        settings.add_custom("Attack", Action::Destroy, (0.3, 0.6));
        let mut l = layout();
        l.settings = settings;

        let centre = l.custom_centre(0);
        assert_eq!(centre, Vec2::new(0.3 * 1600.0, 0.6 * 900.0));
        assert_eq!(l.edit_target_at(centre), Some(EditTarget::Custom(0)));

        // A drag through the editor's setters moves it and saves a centre.
        l.settings
            .set_target_centre(EditTarget::Custom(0), 0.7, 0.2);
        assert_eq!(l.settings.custom[0].placement.centre, Some((0.7, 0.2)));
        l.settings.set_target_size(EditTarget::Custom(0), 1.5);
        assert_eq!(l.custom_radius(0), BUTTON_RADIUS * 2.0 * 1.5);
    }

    #[test]
    fn a_custom_button_takes_edit_priority_over_a_builtin() {
        // Placed on top of the jump button, the custom button wins in the
        // editor, so a freshly added button is always grabbable.
        let jump = layout().button_centre(TouchButton::Jump);
        let mut l = layout();
        let mut settings = TouchLayoutSettings::default();
        settings.add_custom(
            "Attack",
            Action::Destroy,
            (jump.x / l.width, jump.y / l.height),
        );
        l.settings = settings;
        assert_eq!(
            l.edit_target_at(l.custom_centre(0)),
            Some(EditTarget::Custom(0))
        );
    }
}
