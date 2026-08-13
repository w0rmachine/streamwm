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
    use wayland_client;
    use wayland_client::protocol::*;
    use crate::protocols::wm::*;
    use crate::protocols::wm::__interfaces::*;

    pub mod __interfaces {
        #![allow(unused_imports)]
        use wayland_client::protocol::__interfaces::*;
        use crate::protocols::wm::__interfaces::*;
        wayland_scanner::generate_interfaces!("./protocols/stable/river-xkb-bindings-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("./protocols/stable/river-xkb-bindings-v1.xml");
}
