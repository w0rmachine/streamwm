//! Wayland protocol bindings compiled from XML definitions using `wayland-scanner`.
//!
//! Submodules:
//! - [`wm`]: `river-window-management-v1` (handles manage/render passes, node assignment, borders, focus).
//! - [`xkb_bindings`]: `river-xkb-bindings-v1` (handles hotkey registration and event notifications).
//! - [`layer_shell`]: `river-layer-shell-v1` (signals layer-shell support to River compositor).

pub mod wm {
    //! Client-side bindings for `river-window-management-v1`.
    use wayland_client;
    use wayland_client::protocol::*;

    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("./protocols/stable/river-window-management-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("./protocols/stable/river-window-management-v1.xml");
}

pub mod xkb_bindings {
    //! Client-side bindings for `river-xkb-bindings-v1`.
    #![allow(unused_imports)]
    use crate::protocols::wm::__interfaces::*;
    use crate::protocols::wm::*;
    use wayland_client;
    use wayland_client::protocol::*;

    pub mod __interfaces {
        #![allow(unused_imports)]
        use crate::protocols::wm::__interfaces::*;
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("./protocols/stable/river-xkb-bindings-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("./protocols/stable/river-xkb-bindings-v1.xml");
}

pub mod layer_shell {
    //! Client-side bindings for `river-layer-shell-v1`.
    //!
    //! Binding this global interface informs River that `streamwm` supports `wlr-layer-shell`,
    //! allowing external bars (Waybar, Quickshell) and background wallpapers (swaybg) to function.
    #![allow(unused_imports)]
    use wayland_client;
    use wayland_client::protocol::*;
    #[allow(unused_imports)]
    use crate::protocols::wm::__interfaces::*;
    #[allow(unused_imports)]
    use crate::protocols::wm::*;

    pub mod __interfaces {
        #![allow(unused_imports)]
        use crate::protocols::wm::__interfaces::*;
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("./protocols/stable/river-layer-shell-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("./protocols/stable/river-layer-shell-v1.xml");
}
