use blitz_traits::events::{
    BlitzImeEvent, BlitzKeyEvent, BlitzPointerId, KeyState, PointerDetails,
};
use blitz_traits::shell::ColorScheme;
use keyboard_types::{Code, Key, Modifiers, NamedKey};
use winit::event::KeyEvent as WinitKeyEvent;
use winit::event::{ButtonSource, ElementState};
use winit::event::{Ime, PointerKind, PointerSource};
use winit::keyboard::Key as WinitKey;
use winit::keyboard::ModifiersState as WinitModifiers;
use winit::keyboard::PhysicalKey as WinitPhysicalKey;
use winit::window::Theme;

pub(crate) fn theme_to_color_scheme(theme: Theme) -> ColorScheme {
    match theme {
        Theme::Light => ColorScheme::Light,
        Theme::Dark => ColorScheme::Dark,
    }
}

pub(crate) fn color_scheme_to_theme(scheme: ColorScheme) -> Theme {
    match scheme {
        ColorScheme::Light => Theme::Light,
        ColorScheme::Dark => Theme::Dark,
    }
}

pub(crate) fn winit_ime_to_blitz(event: Ime) -> BlitzImeEvent {
    match event {
        Ime::Enabled => BlitzImeEvent::Enabled,
        Ime::Disabled => BlitzImeEvent::Disabled,
        Ime::Preedit(text, cursor) => BlitzImeEvent::Preedit(text, cursor),
        Ime::Commit(text) => BlitzImeEvent::Commit(text),
        Ime::DeleteSurrounding {
            before_bytes,
            after_bytes,
        } => BlitzImeEvent::DeleteSurrounding {
            before_bytes,
            after_bytes,
        },
    }
}

pub(crate) fn winit_key_event_to_blitz(
    event: &WinitKeyEvent,
    mods: WinitModifiers,
) -> BlitzKeyEvent {
    let key = winit_key_to_kbt_key(&event.logical_key);
    let state = match event.state {
        ElementState::Pressed => KeyState::Pressed,
        ElementState::Released => KeyState::Released,
    };
    BlitzKeyEvent {
        modifiers: modifiers_at_key_event(winit_modifiers_to_kbt_modifiers(mods), &key, state),
        key,
        key_without_modifiers: winit_key_to_kbt_key(&event.key_without_modifiers),
        code: winit_physical_key_to_kbt_code(&event.physical_key),
        location: event.location,
        is_auto_repeating: event.repeat,
        is_composing: false,
        state,
        text: event.text.clone(),
    }
}

/// The modifiers current at a key event.
///
/// winit reports `ModifiersChanged` after the key event of a modifier key itself, so
/// the tracked state lags by one event: a Shift press would arrive without Shift and
/// its release with it. A modifier key's own bit is set on press and cleared on release.
fn modifiers_at_key_event(tracked: Modifiers, key: &Key, state: KeyState) -> Modifiers {
    #[allow(deprecated)] // `Super` is the legacy name of `Meta`
    let own = match key {
        Key::Named(NamedKey::Shift) => Modifiers::SHIFT,
        Key::Named(NamedKey::Control) => Modifiers::CONTROL,
        Key::Named(NamedKey::Alt) => Modifiers::ALT,
        Key::Named(NamedKey::Meta | NamedKey::Super) => Modifiers::META,
        _ => return tracked,
    };
    match state {
        KeyState::Pressed => tracked | own,
        KeyState::Released => tracked - own,
    }
}

pub(crate) fn pointer_source_to_blitz(source: &PointerSource) -> BlitzPointerId {
    match source {
        PointerSource::Mouse => BlitzPointerId::Mouse,
        PointerSource::Touch { finger_id, .. } => {
            BlitzPointerId::Finger(finger_id.into_raw() as u64)
        }

        // TODO: TabletTool and Unknown events
        PointerSource::TabletTool { .. } => BlitzPointerId::Pen,
        PointerSource::Unknown => BlitzPointerId::Mouse,
        _ => {
            #[cfg(feature = "tracing")]
            tracing::error!("unsupported PointerSource variant: {source:?}");
            BlitzPointerId::Mouse
        }
    }
}

pub(crate) fn pointer_kind_to_blitz(kind: &PointerKind) -> BlitzPointerId {
    match kind {
        PointerKind::Mouse => BlitzPointerId::Mouse,
        PointerKind::Touch(finger_id) => BlitzPointerId::Finger(finger_id.into_raw() as u64),

        // TODO: TabletTool and Unknown events
        PointerKind::TabletTool(_) => BlitzPointerId::Pen,
        PointerKind::Unknown => BlitzPointerId::Mouse,
        _ => {
            #[cfg(feature = "tracing")]
            tracing::error!("unsupported PointerKind variant: {kind:?}");
            BlitzPointerId::Mouse
        }
    }
}

pub(crate) fn button_source_to_blitz(source: &ButtonSource) -> BlitzPointerId {
    match source {
        ButtonSource::Mouse(_) => BlitzPointerId::Mouse,
        ButtonSource::TabletTool { .. } => BlitzPointerId::Pen,
        ButtonSource::Touch { finger_id, .. } => {
            BlitzPointerId::Finger(finger_id.into_raw() as u64)
        }

        // TODO: Unknown events
        ButtonSource::Unknown(_) => BlitzPointerId::Mouse,
        _ => {
            #[cfg(feature = "tracing")]
            tracing::error!("unsupported ButtonSource variant: {source:?}");
            BlitzPointerId::Mouse
        }
    }
}

pub(crate) fn pointer_source_to_blitz_details(source: &PointerSource) -> PointerDetails {
    match source {
        PointerSource::Mouse => PointerDetails::default(),
        PointerSource::Unknown => PointerDetails::default(),
        PointerSource::Touch { force, .. } => PointerDetails {
            pressure: force.map(|force| force.normalized(None)).unwrap_or(0.0),
            ..PointerDetails::default()
        },
        PointerSource::TabletTool { data, .. } => PointerDetails {
            pressure: data
                .force
                .map(|force| force.normalized(data.angle))
                .unwrap_or(0.0),
            tangential_pressure: data.tangential_force.unwrap_or(0.0),
            tilt_x: data.tilt.map(|tilt| tilt.x).unwrap_or(0),
            tilt_y: data.tilt.map(|tilt| tilt.y).unwrap_or(0),
            twist: data.twist.unwrap_or(0),
            altitude: data.angle.map(|angle| angle.altitude).unwrap_or(0.0),
            azimuth: data.angle.map(|angle| angle.azimuth).unwrap_or(0.0),
        },
        _ => {
            #[cfg(feature = "tracing")]
            tracing::error!("unsupported PointerSource variant: {source:?}");
            PointerDetails::default()
        }
    }
}

pub(crate) fn winit_modifiers_to_kbt_modifiers(winit_modifiers: WinitModifiers) -> Modifiers {
    let mut modifiers = Modifiers::default();
    if winit_modifiers.control_key() {
        modifiers.insert(Modifiers::CONTROL);
    }
    if winit_modifiers.alt_key() {
        modifiers.insert(Modifiers::ALT);
    }
    if winit_modifiers.shift_key() {
        modifiers.insert(Modifiers::SHIFT);
    }
    if winit_modifiers.meta_key() {
        modifiers.insert(Modifiers::META);
    }
    modifiers
}

pub(crate) fn winit_physical_key_to_kbt_code(physical_key: &WinitPhysicalKey) -> Code {
    match physical_key {
        WinitPhysicalKey::Code(code) => *code,
        WinitPhysicalKey::Unidentified(_) => Code::Unidentified,
    }
}

pub(crate) fn winit_key_to_kbt_key(winit_key: &WinitKey) -> Key {
    match winit_key {
        WinitKey::Named(named_key) => Key::Named(*named_key),
        WinitKey::Character(c) => Key::Character(c.to_string()),
        WinitKey::Unidentified(_) => Key::Named(NamedKey::Unidentified),
        WinitKey::Dead(_) => Key::Named(NamedKey::Dead),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_modifier_press_carries_its_own_bit() {
        let key = Key::Named(NamedKey::Shift);
        let mods = modifiers_at_key_event(Modifiers::empty(), &key, KeyState::Pressed);
        assert_eq!(mods, Modifiers::SHIFT);
    }

    #[test]
    fn a_modifier_release_drops_its_own_bit_and_keeps_the_others() {
        let held = Modifiers::SHIFT | Modifiers::CONTROL;
        let key = Key::Named(NamedKey::Shift);
        let mods = modifiers_at_key_event(held, &key, KeyState::Released);
        assert_eq!(mods, Modifiers::CONTROL);
    }

    #[test]
    fn an_ordinary_key_keeps_the_tracked_modifiers() {
        let key = Key::Named(NamedKey::Enter);
        let mods = modifiers_at_key_event(Modifiers::ALT, &key, KeyState::Pressed);
        assert_eq!(mods, Modifiers::ALT);
    }

    #[test]
    fn meta_sets_the_meta_bit() {
        let key = Key::Named(NamedKey::Meta);
        let mods = modifiers_at_key_event(Modifiers::empty(), &key, KeyState::Pressed);
        assert_eq!(mods, Modifiers::META);
    }
}
