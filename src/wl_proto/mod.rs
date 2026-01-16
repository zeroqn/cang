mod debug;
mod map;
mod socket;
mod wire;

use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::sync::OnceLock;

use calloop::{
    EventSource, Interest, Poll, PostAction, Readiness, Token, TokenFactory, generic::Generic,
};
use log::{info, warn};
pub use wayland_server::backend::protocol;
use wayland_server::backend::protocol::{Argument, Interface, Message, same_interface};
use wayland_server::protocol::__interfaces::{WL_DISPLAY_INTERFACE, WL_REGISTRY_INTERFACE};

pub use self::map::{Object, ObjectMap};
pub use self::socket::{Buffer, MAX_BYTES_OUT, MAX_FDS_OUT};
use self::socket::{BufferedSocket, Socket};
pub use self::wire::{MessageParseError, TryClone, parse_message, write_to_buffers};

#[derive(Debug, Clone)]
pub struct Data;

pub struct ClientConnection {
    socket: Generic<BufferedSocket>,
    pub map: ObjectMap<Data>,
    filters: Vec<String>,
    globals: HashMap<u32, (&'static Interface, u32)>,
    debug: bool,
}

impl ClientConnection {
    pub fn new(stream: UnixStream, filters: impl IntoIterator<Item = String>) -> Self {
        let socket = BufferedSocket::new(Socket::from(stream));
        let mut map = ObjectMap::new();
        map.insert_at(
            1,
            Object {
                interface: &WL_DISPLAY_INTERFACE,
                version: 1,
                data: Data,
            },
        )
        .unwrap();

        ClientConnection {
            socket: Generic::new(socket, Interest::READ, calloop::Mode::Level),
            map,
            filters: Vec::from_iter(filters),
            globals: HashMap::new(),
            debug: self::debug::has_debug_client_env(),
        }
    }

    pub fn write_message(&mut self, msg: &Message<u32, OwnedFd>) -> std::io::Result<()> {
        let obj = self.map.find(msg.sender_id).unwrap();
        let desc = obj.interface.events.get(msg.opcode as usize).unwrap();

        if same_interface(&obj.interface, &WL_DISPLAY_INTERFACE) && msg.opcode == 1 {
            // wl_display::delete_id(id: uint)
            let &[Argument::Uint(ref id)] = msg.args.as_slice() else {
                unreachable!()
            };
            self.map.remove(*id);
        }
        if same_interface(&obj.interface, &WL_REGISTRY_INTERFACE) && msg.opcode == 0 {
            // wl_registry::global(name: uint, interface: string, version: uint)
            let &[
                Argument::Uint(ref id),
                Argument::Str(Some(ref interface_name)),
                Argument::Uint(ref version),
            ] = msg.args.as_slice()
            else {
                unreachable!()
            };

            if let Some((interface, version)) = match interface_name.to_str() {
                Ok(name) => {
                    if self.filters.iter().any(|n| n == name) {
                        info!("Filtering global: {}.", name);
                        return Ok(());
                    } else {
                        global_to_interface(name, *version)
                    }
                }
                Err(_) => None,
            } {
                self.globals.insert(*id, (interface, version));
            } else {
                warn!(
                    "Unknown interface {:?}. Hiding global.",
                    interface_name.to_string_lossy() // FIXME(nightly): cstr_display
                );
                return Ok(());
            }
        }
        if same_interface(&obj.interface, &WL_REGISTRY_INTERFACE) && msg.opcode == 1 {
            // wl_registry::global_remove(name: uint)

            let &[Argument::Uint(ref id)] = msg.args.as_slice() else {
                unreachable!()
            };

            self.globals.remove(id);
        }

        for arg in &msg.args {
            match arg {
                Argument::NewId(id) => {
                    let child = Object {
                        interface: match desc.child_interface {
                            Some(iface) => iface,
                            None => panic!(
                                "Received request {}@{}.{} which creates an object without specifying its interface, this is unsupported.",
                                obj.interface.name, msg.sender_id, desc.name
                            ),
                        },
                        version: obj.version,
                        data: Data,
                    };
                    if let Err(()) = self.map.insert_at(*id, child) {
                        unsafe { self.socket.get_mut() }.write_message(display_error())?;
                        unsafe { self.socket.get_mut() }.flush()?;
                        return Err(std::io::ErrorKind::InvalidData.into());
                    }
                }
                _ => {}
            }
        }

        if self.debug {
            self::debug::print_send_message(
                obj.interface.name,
                msg.sender_id,
                obj.interface.events.get(msg.opcode as usize).unwrap().name,
                &msg.args,
                false,
            );
        }

        unsafe { self.socket.get_mut() }.write_message(msg)
    }

    pub fn flush(&mut self) -> std::io::Result<()> {
        unsafe { self.socket.get_mut() }.flush()
    }
}

fn display_error() -> &'static Message<u32, OwnedFd> {
    // wl_display::error(object_id: object, code: uint, message: string)
    static DISPLAY_ERROR: OnceLock<Message<u32, OwnedFd>> = OnceLock::new();

    DISPLAY_ERROR.get_or_init(|| Message {
        sender_id: 1,
        opcode: 0,
        args: smallvec::smallvec![
            Argument::Object(1),
            Argument::Uint(0),
            Argument::Str(Some(Box::new(c"Error adding new_id".to_owned()))),
        ],
    })
}

fn next_message(
    socket: &mut BufferedSocket,
    map: &ObjectMap<Data>,
    debug: bool,
) -> std::io::Result<(Message<u32, OwnedFd>, Object<Data>)> {
    loop {
        let msg = match socket.read_one_message(|id, opcode| {
            map.find(id)
                .and_then(|o| o.interface.requests.get(opcode as usize))
                .map(|desc| desc.signature)
        }) {
            Ok(msg) => msg,
            Err(MessageParseError::MissingData) | Err(MessageParseError::MissingFD) => {
                // need to read more data
                if let Err(e) = socket.fill_incoming_buffers() {
                    return Err(e);
                }
                continue;
            }
            Err(MessageParseError::Malformed) => {
                return Err(rustix::io::Errno::PROTO.into());
            }
        };

        let obj = map.find(msg.sender_id).unwrap();

        if debug {
            self::debug::print_dispatched_message(
                obj.interface.name,
                msg.sender_id,
                obj.interface
                    .requests
                    .get(msg.opcode as usize)
                    .unwrap()
                    .name,
                &msg.args,
            );
        }

        return Ok((msg, obj));
    }
}

impl EventSource for ClientConnection {
    type Event = Message<u32, OwnedFd>;
    type Metadata = Object<Data>;
    type Ret = Result<PostAction, anyhow::Error>;
    type Error = std::io::Error;

    fn process_events<F>(
        &mut self,
        readiness: Readiness,
        token: Token,
        mut callback: F,
    ) -> Result<PostAction, std::io::Error>
    where
        F: FnMut(Self::Event, &mut Self::Metadata) -> Self::Ret,
    {
        self.socket.process_events(readiness, token, |_, socket| {
            loop {
                // SAFETY `next_message` doesn't exchange the socket
                let (msg, mut obj) = match next_message(unsafe { socket.get_mut() }, &self.map, self.debug) {
                    Ok((msg, obj)) => (msg, obj),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        break;
                    }
                    Err(e) => {
                        warn!("Wayland connection died: {:?}", e);
                        return Ok(PostAction::Remove);
                    }
                };

                if same_interface(&obj.interface, &WL_REGISTRY_INTERFACE) && msg.opcode == 0 {
                    // wl_registry.bind(uint name, str interface, uint version, new id)
                    if let [Argument::Uint(name), Argument::Str(Some(_interface_name)), Argument::Uint(version), Argument::NewId(new_id)] =
                        &msg.args[..]
                    {
                        let Some((interface, max)) = self.globals.get(name) else {
                            return Err(std::io::ErrorKind::InvalidData.into());
                        };
                        if version > max {
                            return Err(std::io::ErrorKind::InvalidData.into());
                        }
                        let object = Object {
                           interface,
                           version: *version,
                           data: Data,
                        };
                        if let Err(()) = self.map.insert_at(*new_id, object) {
                            unsafe { socket.get_mut() }.write_message(display_error())?;
                            unsafe { socket.get_mut() }.flush()?;
                            return Ok(PostAction::Remove);
                        }
                    }
                } else {
                    let desc = obj.interface.requests.get(msg.opcode as usize).unwrap();
                    for arg in &msg.args {
                        match arg {
                            Argument::NewId(id) => {
                                let child = Object {
                                    interface: match desc.child_interface {
                                        Some(iface) => iface,
                                        None => panic!("Received request {}@{}.{} which creates an object without specifying its interface, this is unsupported.", obj.interface.name, msg.sender_id, desc.name),
                                    },
                                    version: obj.version,
                                    data: Data,
                                };
                                if let Err(()) = self.map.insert_at(*id, child) {
                                    unsafe { socket.get_mut() }.write_message(display_error())?;
                                    unsafe { socket.get_mut() }.flush()?;
                                    return Ok(PostAction::Remove);
                                }
                            },
                            _ => {},
                        }
                    }
                    // DO NOT delete if this is a destructor. Deletion must be ACKed by the server with delete_id.
                }

                if let Err(err) = callback(msg, &mut obj) {
                    warn!("Wayland message handler error: {:?}", err);
                };
            }

            Ok(PostAction::Continue)
        })
    }

    fn register(
        &mut self,
        poll: &mut Poll,
        token_factory: &mut TokenFactory,
    ) -> calloop::Result<()> {
        self.socket.register(poll, token_factory)
    }

    fn reregister(
        &mut self,
        poll: &mut Poll,
        token_factory: &mut TokenFactory,
    ) -> calloop::Result<()> {
        self.socket.reregister(poll, token_factory)
    }

    fn unregister(&mut self, poll: &mut Poll) -> calloop::Result<()> {
        self.socket.unregister(poll)
    }
}

fn global_to_interface(interface_name: &str, version: u32) -> Option<(&'static Interface, u32)> {
    use wayland_protocols::ext::background_effect::v1::server::__interfaces::EXT_BACKGROUND_EFFECT_MANAGER_V1_INTERFACE;
    use wayland_protocols::ext::data_control::v1::server::__interfaces::EXT_DATA_CONTROL_MANAGER_V1_INTERFACE;
    use wayland_protocols::ext::foreign_toplevel_list::v1::server::__interfaces::EXT_FOREIGN_TOPLEVEL_LIST_V1_INTERFACE;
    use wayland_protocols::ext::idle_notify::v1::server::__interfaces::EXT_IDLE_NOTIFIER_V1_INTERFACE;
    use wayland_protocols::ext::transient_seat::v1::server::__interfaces::EXT_TRANSIENT_SEAT_MANAGER_V1_INTERFACE;
    use wayland_protocols::ext::workspace::v1::server::__interfaces::EXT_WORKSPACE_MANAGER_V1_INTERFACE;
    use wayland_protocols::wp::alpha_modifier::v1::server::__interfaces::WP_ALPHA_MODIFIER_V1_INTERFACE;
    use wayland_protocols::wp::color_management::v1::server::__interfaces::WP_COLOR_MANAGER_V1_INTERFACE;
    use wayland_protocols::wp::color_representation::v1::server::__interfaces::WP_COLOR_REPRESENTATION_MANAGER_V1_INTERFACE;
    use wayland_protocols::wp::commit_timing::v1::server::__interfaces::WP_COMMIT_TIMING_MANAGER_V1_INTERFACE;
    use wayland_protocols::wp::content_type::v1::server::__interfaces::WP_CONTENT_TYPE_MANAGER_V1_INTERFACE;
    use wayland_protocols::wp::cursor_shape::v1::server::__interfaces::WP_CURSOR_SHAPE_MANAGER_V1_INTERFACE;
    use wayland_protocols::wp::fifo::v1::server::__interfaces::WP_FIFO_MANAGER_V1_INTERFACE;
    use wayland_protocols::wp::fractional_scale::v1::server::__interfaces::WP_FRACTIONAL_SCALE_MANAGER_V1_INTERFACE;
    use wayland_protocols::wp::idle_inhibit::zv1::server::__interfaces::ZWP_IDLE_INHIBIT_MANAGER_V1_INTERFACE;
    use wayland_protocols::wp::input_method::zv1::server::__interfaces::ZWP_INPUT_METHOD_CONTEXT_V1_INTERFACE;
    use wayland_protocols::wp::input_timestamps::zv1::server::__interfaces::ZWP_INPUT_TIMESTAMPS_MANAGER_V1_INTERFACE;
    use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::server::__interfaces::ZWP_KEYBOARD_SHORTCUTS_INHIBIT_MANAGER_V1_INTERFACE;
    use wayland_protocols::wp::linux_dmabuf::zv1::server::__interfaces::ZWP_LINUX_DMABUF_V1_INTERFACE;
    use wayland_protocols::wp::pointer_constraints::zv1::server::__interfaces::ZWP_POINTER_CONSTRAINTS_V1_INTERFACE;
    use wayland_protocols::wp::pointer_gestures::zv1::server::__interfaces::ZWP_POINTER_GESTURES_V1_INTERFACE;
    use wayland_protocols::wp::pointer_warp::v1::server::__interfaces::WP_POINTER_WARP_V1_INTERFACE;
    use wayland_protocols::wp::presentation_time::server::__interfaces::WP_PRESENTATION_INTERFACE;
    use wayland_protocols::wp::primary_selection::zv1::server::__interfaces::ZWP_PRIMARY_SELECTION_DEVICE_MANAGER_V1_INTERFACE;
    use wayland_protocols::wp::relative_pointer::zv1::server::__interfaces::ZWP_RELATIVE_POINTER_MANAGER_V1_INTERFACE;
    use wayland_protocols::wp::single_pixel_buffer::v1::server::__interfaces::WP_SINGLE_PIXEL_BUFFER_MANAGER_V1_INTERFACE;
    use wayland_protocols::wp::tablet::zv2::server::__interfaces::ZWP_TABLET_MANAGER_V2_INTERFACE;
    use wayland_protocols::wp::text_input::zv3::server::__interfaces::ZWP_TEXT_INPUT_MANAGER_V3_INTERFACE;
    use wayland_protocols::wp::viewporter::server::__interfaces::WP_VIEWPORTER_INTERFACE;
    use wayland_protocols::xdg::activation::v1::server::__interfaces::XDG_ACTIVATION_V1_INTERFACE;
    use wayland_protocols::xdg::decoration::zv1::server::__interfaces::ZXDG_DECORATION_MANAGER_V1_INTERFACE;
    use wayland_protocols::xdg::dialog::v1::server::__interfaces::XDG_WM_DIALOG_V1_INTERFACE;
    use wayland_protocols::xdg::foreign::zv2::server::__interfaces::ZXDG_EXPORTER_V2_INTERFACE;
    use wayland_protocols::xdg::shell::server::__interfaces::XDG_WM_BASE_INTERFACE;
    use wayland_protocols::xdg::system_bell::v1::server::__interfaces::XDG_SYSTEM_BELL_V1_INTERFACE;
    use wayland_protocols::xdg::toplevel_drag::v1::server::__interfaces::XDG_TOPLEVEL_DRAG_MANAGER_V1_INTERFACE;
    use wayland_protocols::xdg::toplevel_icon::v1::server::__interfaces::XDG_TOPLEVEL_ICON_MANAGER_V1_INTERFACE;
    use wayland_protocols::xdg::toplevel_tag::v1::server::__interfaces::XDG_TOPLEVEL_TAG_MANAGER_V1_INTERFACE;
    use wayland_protocols::xdg::xdg_output::zv1::server::__interfaces::ZXDG_OUTPUT_MANAGER_V1_INTERFACE;
    use wayland_protocols_wlr::layer_shell::v1::server::__interfaces::ZWLR_LAYER_SHELL_V1_INTERFACE;
    use wayland_server::protocol::__interfaces::{
        WL_COMPOSITOR_INTERFACE, WL_DATA_DEVICE_MANAGER_INTERFACE, WL_FIXES_INTERFACE,
        WL_OUTPUT_INTERFACE, WL_SEAT_INTERFACE, WL_SHELL_INTERFACE, WL_SHM_INTERFACE,
        WL_SUBCOMPOSITOR_INTERFACE,
    };

    let (interface, max) = match interface_name {
        "wl_compositor" => (&WL_COMPOSITOR_INTERFACE, 6),
        "wl_subcompositor" => (&WL_SUBCOMPOSITOR_INTERFACE, 1),
        "wl_shm" => (&WL_SHM_INTERFACE, 2),
        "wl_data_device_manager" => (&WL_DATA_DEVICE_MANAGER_INTERFACE, 3),
        "wl_shell" => (&WL_SHELL_INTERFACE, 1),
        "wl_seat" => (&WL_SEAT_INTERFACE, 10),
        "wl_output" => (&WL_OUTPUT_INTERFACE, 4),
        "wl_fixes" => (&WL_FIXES_INTERFACE, 1),
        "wp_presentation" => (&WP_PRESENTATION_INTERFACE, 2),
        "wp_viewporter" => (&WP_VIEWPORTER_INTERFACE, 1),
        "xdg_wm_base" => (&XDG_WM_BASE_INTERFACE, 7),
        "zwp_linux_dmabuf_v1" => (&ZWP_LINUX_DMABUF_V1_INTERFACE, 5),
        "xdg_activation_v1" => (&XDG_ACTIVATION_V1_INTERFACE, 1),
        "wp_single_pixel_buffer_manager_v1" => (&WP_SINGLE_PIXEL_BUFFER_MANAGER_V1_INTERFACE, 1),
        "wp_content_type_manager_v1" => (&WP_CONTENT_TYPE_MANAGER_V1_INTERFACE, 1),
        "ext_idle_notifier_v1" => (&EXT_IDLE_NOTIFIER_V1_INTERFACE, 2),
        "wp_fractional_scale_manager_v1" => (&WP_FRACTIONAL_SCALE_MANAGER_V1_INTERFACE, 1),
        "wp_cursor_shape_manager_v1" => (&WP_CURSOR_SHAPE_MANAGER_V1_INTERFACE, 2),
        "ext_foreign_toplevel_list_v1" => (&EXT_FOREIGN_TOPLEVEL_LIST_V1_INTERFACE, 1),
        "ext_transient_seat_manager_v1" => (&EXT_TRANSIENT_SEAT_MANAGER_V1_INTERFACE, 1),
        "xdg_toplevel_drag_manager_v1" => (&XDG_TOPLEVEL_DRAG_MANAGER_V1_INTERFACE, 1),
        "xdg_toplevel_icon_manager_v1" => (&XDG_TOPLEVEL_ICON_MANAGER_V1_INTERFACE, 1),
        "xdg_wm_dialog_v1" => (&XDG_WM_DIALOG_V1_INTERFACE, 1),
        "wp_alpha_modifier_v1" => (&WP_ALPHA_MODIFIER_V1_INTERFACE, 1),
        "wp_commit_timing_manager_v1" => (&WP_COMMIT_TIMING_MANAGER_V1_INTERFACE, 1),
        "ext_data_control_manager_v1" => (&EXT_DATA_CONTROL_MANAGER_V1_INTERFACE, 1),
        "wp_fifo_manager_v1" => (&WP_FIFO_MANAGER_V1_INTERFACE, 1),
        "xdg_system_bell_v1" => (&XDG_SYSTEM_BELL_V1_INTERFACE, 1),
        "ext_workspace_manager_v1" => (&EXT_WORKSPACE_MANAGER_V1_INTERFACE, 1),
        "wp_color_manager_v1" => (&WP_COLOR_MANAGER_V1_INTERFACE, 1),
        "wp_color_representation_manager_v1" => (&WP_COLOR_REPRESENTATION_MANAGER_V1_INTERFACE, 1),
        "xdg_toplevel_tag_manager_v1" => (&XDG_TOPLEVEL_TAG_MANAGER_V1_INTERFACE, 1),
        "wp_pointer_warp_v1" => (&WP_POINTER_WARP_V1_INTERFACE, 1),
        "ext_background_effect_manager_v1" => (&EXT_BACKGROUND_EFFECT_MANAGER_V1_INTERFACE, 1),
        "zwp_idle_inhibit_manager_v1" => (&ZWP_IDLE_INHIBIT_MANAGER_V1_INTERFACE, 1),
        "zwp_input_method_context_v1" => (&ZWP_INPUT_METHOD_CONTEXT_V1_INTERFACE, 1),
        "zwp_input_timestamps_manager_v1" => (&ZWP_INPUT_TIMESTAMPS_MANAGER_V1_INTERFACE, 1),
        "zwp_keyboard_shortcuts_inhibit_manager_v1" => {
            (&ZWP_KEYBOARD_SHORTCUTS_INHIBIT_MANAGER_V1_INTERFACE, 1)
        }
        "zwp_pointer_constraints_v1" => (&ZWP_POINTER_CONSTRAINTS_V1_INTERFACE, 1),
        "zwp_pointer_gestures_v1" => (&ZWP_POINTER_GESTURES_V1_INTERFACE, 3),
        "zwp_primary_selection_device_manager_v1" => {
            (&ZWP_PRIMARY_SELECTION_DEVICE_MANAGER_V1_INTERFACE, 1)
        }
        "zwp_relative_pointer_manager_v1" => (&ZWP_RELATIVE_POINTER_MANAGER_V1_INTERFACE, 1),
        "zwp_tablet_manager_v2" => (&ZWP_TABLET_MANAGER_V2_INTERFACE, 2),
        "zwp_text_input_manager_v3" => (&ZWP_TEXT_INPUT_MANAGER_V3_INTERFACE, 1),
        "zxdg_decoration_manager_v1" => (&ZXDG_DECORATION_MANAGER_V1_INTERFACE, 1),
        "zxdg_exporter_v2" => (&ZXDG_EXPORTER_V2_INTERFACE, 1),
        "zxdg_output_manager_v1" => (&ZXDG_OUTPUT_MANAGER_V1_INTERFACE, 3),
        "zwlr_layer_shell_v1" => (&ZWLR_LAYER_SHELL_V1_INTERFACE, 5),
        name => {
            warn!("Filtering out unknown global interface: {}", name);
            return None;
        }
    };

    if max < version {
        warn!(
            "Max supported version ({}) for global {} is lower than compositor advertised one ({}), downgrading version.",
            max, interface_name, version
        );
    }

    Some((interface, version.min(max)))
}
