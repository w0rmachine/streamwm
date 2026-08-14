//! Protocol bindings: generated from the river protocol XMLs, using the
//! wayland-scanner proc macros.

pub mod wm {
    //! river-window-management-v1 (client side).
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
    //! river-xkb-bindings-v1 (client side). References river_seat_v1 and
    //! wl_seat from river-window-management-v1 / core.
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
    //! river-layer-shell-v1 (client side). Binding this global signals to the
    //! compositor that the window manager supports wlr-layer-shell, allowing
    //! clients (quickshell bar, swaybg background) to map layer surfaces.
    #![allow(unused_imports)]
    use wayland_client;
    use wayland_client::protocol::*;
    // The generated client code references river_output_v1 / river_seat_v1 from
    // river-window-management-v1; bring them into scope for the macro.
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
