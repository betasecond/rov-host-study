use std::{cell::{Cell, RefCell}, collections::HashMap, net::Ipv4Addr, rc::Rc};

use fragile::Fragile;
use glib::{MainContext, PRIORITY_DEFAULT, PRIORITY_HIGH, Type, clone, Sender, WeakRef};

use gstreamer as gst;

use gtk::{AboutDialog, Align, Box as GtkBox, Button, CenterBox, Frame, Grid, Image, Inhibit, Label, MenuButton, Orientation, Stack, ToggleButton, gio::{Menu, MenuItem}, prelude::*};

use adw::{ApplicationWindow, CenteringPolicy, ColorScheme, HeaderBar, StatusPage, StyleManager, prelude::*, Window};

use input::{InputEvent, InputSource, InputSourceEvent, InputSystem};
use preferences::PreferencesModel;
use relm4::{AppUpdate, ComponentUpdate, Components, Model, RelmApp, RelmComponent, Widgets, actions::{ActionGroupName, ActionName, RelmAction, RelmActionGroup}, factory::{DynamicIndex, FactoryPrototype, FactoryVec, FactoryVecDeque, positions::GridPosition}, send, new_stateful_action, new_stateless_action, new_action_group};
use relm4_macros::widget;
use lazy_static::{__Deref, lazy_static};
use slave::{MyComponent, SlaveMsg};

mod preferences;
mod slave;
mod components;
mod prelude;
mod video;
mod input;
mod graph_view;

use crate::{preferences::PreferencesMsg, slave::{SlaveConfigModel, SlaveConfigMsg, SlaveModel, SlaveStatusClass, SlaveVideoMsg}};

use sdl2::{JoystickSubsystem, Sdl, event::Event, joystick::Joystick};

use derivative::*;

struct AboutModel {}
enum AboutMsg {}
impl Model for AboutModel {
    type Msg = AboutMsg;
    type Widgets = AboutWidgets;
    type Components = ();
}

#[widget]
impl Widgets<AboutModel, AppModel> for AboutWidgets {
    view! {
        dialog = AboutDialog {
            set_transient_for: parent!(Some(&parent_widgets.app_window)),
            set_destroy_with_parent: true,
            set_modal: true,
            connect_close_request(sender) => move |window| {
                window.hide();
                Inhibit(true)
            },
            set_authors: &["黄博宏"],
            set_program_name: Some("水下机器人上位机"),
            set_copyright: Some("© 2021 集美大学水下智能创新实验室"),
            set_comments: Some("跨平台的校园水下机器人上位机程序"),
            set_logo_icon_name: Some("applications-games"),
            set_version: Some("0.0.1"),
        }
    }
}

impl ComponentUpdate<AppModel> for AboutModel {
    fn init_model(parent_model: &AppModel) -> Self { AboutModel {} }
    fn update(&mut self, msg: AboutMsg, components: &(), sender: Sender<AboutMsg>, parent_sender: Sender<AppMsg>) {}
}

#[tracker::track]
#[derive(Derivative)]
#[derivative(Default)]
pub struct AppModel {
    recording: Option<bool>,
    #[no_eq]
    #[derivative(Default(value="FactoryVec::new()"))]
    slaves: FactoryVec<MyComponent<SlaveModel>>,
    // #[no_eq]
    // slave_input_event_senders: Rc<RefCell<Vec<Sender<InputSourceEvent>>>>,
    #[no_eq]
    preferences: Rc<RefCell<PreferencesModel>>,
    #[no_eq]
    input_system: Rc<InputSystem>,
}

impl Model for AppModel {
    type Msg = AppMsg;
    type Widgets = AppWidgets;
    type Components = AppComponents;
}

new_action_group!(AppActionGroup, "main");
new_stateless_action!(PreferencesAction, AppActionGroup, "preferences");
new_stateless_action!(KeybindingsAction, AppActionGroup, "keybindings");
new_stateless_action!(AboutDialogAction, AppActionGroup, "about");

fn application_window() -> ApplicationWindow {
    ApplicationWindow::builder().build()
}

#[widget(pub)]
impl Widgets<AppModel, ()> for AppWidgets {
    view! {
        app_window = application_window() -> ApplicationWindow {
            set_title: Some("水下机器人上位机"),
            set_default_width: 1280,
            set_default_height: 720, 
            set_content = Some(&GtkBox) {
                set_orientation: Orientation::Vertical,
                append = &HeaderBar {
                    set_centering_policy: CenteringPolicy::Strict,
                    pack_start = &Button {
                        set_halign: Align::Center,
                        set_css_classes?: watch!(model.recording.map(|x| if x { &["destructive-action"] as &[&str] } else { &[] as &[&str] })),
                        set_child = Some(&GtkBox) {
                            set_spacing: 6,
                            append = &Image {
                                set_icon_name?: watch!(model.recording.map(|x| Some(if x { "media-playback-stop-symbolic" } else { "media-record-symbolic" })))
                            },
                            append = &Label {
                                set_label?: watch!(model.recording.map(|x| if x { "停止" } else { "录制" })),
                            },
                        },
                        set_visible: watch!(model.recording != None),
                        connect_clicked(sender) => move |button| {
                            send!(sender, AppMsg::ToggleRecording);
                        }
                    },
                    pack_end = &MenuButton {
                        set_menu_model: Some(&main_menu),
                        set_icon_name: "open-menu-symbolic",
                        set_focus_on_click: false,
                        set_valign: Align::Center,
                    },
                    pack_end = &Button {
                        set_icon_name: "night-light-symbolic",
                        set_tooltip_text: Some("切换配色方案"),
                        connect_clicked(sender) => move |button| {
                            send!(sender, AppMsg::SwitchColorScheme);
                        }
                    },
                    pack_end = &Button {
                        set_icon_name: "list-remove-symbolic",
                        set_tooltip_text: Some("移除机位"),
                        set_sensitive: track!(model.changed(AppModel::recording()) || model.changed(AppModel::slaves()), model.get_slaves().len() > 0 && *model.get_recording() != Some(true)),
                        connect_clicked(sender) => move |button| {
                            send!(sender, AppMsg::DestroySlave(std::ptr::null()));
                        },
                    },
                    pack_end = &Button {
                        set_icon_name: "tab-new-symbolic",
                        set_tooltip_text: Some("新建机位"),
                        set_sensitive: track!(model.changed(AppModel::recording()), model.recording != Some(true)),
                        connect_clicked[sender = sender.clone(), window = app_window.clone().downgrade()] => move |button| {
                            send!(sender, AppMsg::NewSlave(window.clone()));
                        },
                    },
                },
                append: body_stack = &Stack  {
                    add_child: welcome_page = &StatusPage {
                        set_icon_name: Some("tab-new-symbolic"),
                        set_title: "无机位",
                        set_description: Some("请点击标题栏右侧按钮添加机位"),
                    },
                    add_child: slaves_page = &Grid {
                        set_hexpand: true,
                        set_vexpand: true,
                        factory!(model.slaves),
                    },
                },
            },
            connect_close_request(sender) => move |window| {
                send!(sender, AppMsg::StopInputSystem);
                Inhibit(false)
            },
        }
    }

    menu! {
        main_menu: {
            "首选项"     => PreferencesAction,
            "键盘快捷键" => KeybindingsAction,
            "关于"       => AboutDialogAction,
        }
    }

    fn post_view() {
        
        if model.changed(AppModel::slaves()) {
            if model.get_slaves().len() == 0 {
                self.body_stack.set_visible_child(&self.welcome_page);
            } else {
                self.body_stack.set_visible_child(&self.slaves_page);
            }
        }
    }
    
    fn post_init() {
        let app_group = RelmActionGroup::<AppActionGroup>::new();
        
        let action_preferences: RelmAction<PreferencesAction> = RelmAction::new_stateless(clone!(@strong sender => move |_| {
            send!(sender, AppMsg::OpenPreferencesWindow);
        }));
        let action_keybindings: RelmAction<KeybindingsAction> = RelmAction::new_stateless(clone!(@strong sender => move |_| {
            send!(sender, AppMsg::OpenKeybindingsWindow);
        }));
        let action_about: RelmAction<AboutDialogAction> = RelmAction::new_stateless(clone!(@strong sender => move |_| {
            send!(sender, AppMsg::OpenAboutDialog);
        }));
        
        app_group.add_action(action_preferences);
        app_group.add_action(action_keybindings);
        app_group.add_action(action_about);
        app_window.insert_action_group("main", Some(&app_group.into_action_group()));
        for _ in 0..*model.preferences.borrow().get_initial_slave_num() {
            send!(sender, AppMsg::NewSlave(app_window.clone().downgrade()));
        }
        
        let (input_event_sender, input_event_receiver) = MainContext::channel(PRIORITY_DEFAULT);
        *model.input_system.event_sender.borrow_mut() = Some(input_event_sender);
        
        input_event_receiver.attach(None, clone!(@strong sender => move |event| {
            send!(sender, AppMsg::DispatchInputEvent(event));
            Continue(true)
        }));
    }
}

pub enum AppMsg {
    NewSlave(WeakRef<ApplicationWindow>),
    DestroySlave(*const SlaveModel),
    DispatchInputEvent(InputEvent),
    PreferencesUpdated(PreferencesModel),
    ToggleRecording, 
    OpenAboutDialog,
    OpenPreferencesWindow,
    OpenKeybindingsWindow,
    SwitchColorScheme,
    StopInputSystem, 
}

#[derive(relm4_macros::Components)]
pub struct AppComponents {
    about: RelmComponent::<AboutModel, AppModel>,
    preferences: RelmComponent::<PreferencesModel, AppModel>,
}


impl AppUpdate for AppModel {
    fn update(
        &mut self,
        msg: AppMsg,
        components: &AppComponents,
        sender: Sender<AppMsg>,
    ) -> bool {
        match msg {
            AppMsg::OpenAboutDialog => {
                components.about.root_widget().present();
            },
            AppMsg::OpenPreferencesWindow => {
                components.preferences.root_widget().present();
            },
            AppMsg::OpenKeybindingsWindow => todo!(),
            AppMsg::NewSlave(app_window) => {
                let mut ip_octets = self.get_preferences().borrow().get_default_slave_ipv4_address().octets();
                let index = self.get_slaves().len() as u8;
                ip_octets[3] = ip_octets[3].wrapping_add(index);
                let video_port = self.get_preferences().borrow().get_default_local_video_port().wrapping_add(index as u16);
                let (input_event_sender, input_event_receiver) = MainContext::channel(PRIORITY_DEFAULT);
                let (slave_event_sender, slave_event_receiver) = MainContext::channel(PRIORITY_DEFAULT);
                let mut slave_config = SlaveConfigModel::new(Ipv4Addr::from(ip_octets), *self.get_preferences().clone().borrow().get_default_slave_port(), video_port);
                slave_config.set_keep_video_display_ratio(*self.get_preferences().borrow().get_default_keep_video_display_ratio());
                let slave = SlaveModel::new(slave_config, self.get_preferences().clone(), &slave_event_sender, input_event_sender);
                let component = MyComponent::new(slave, (sender.clone(), app_window));
                let component_sender = component.sender().clone();
                input_event_receiver.attach(None,  clone!(@strong component_sender => move |event| {
                    component_sender.send(SlaveMsg::InputReceived(event)).unwrap();
                    Continue(true)
                }));
                slave_event_receiver.attach(None, clone!(@strong component_sender => move |event| {
                    component_sender.send(event).unwrap();
                    Continue(true)
                }));
                self.get_mut_slaves().push(component);
                self.set_recording(Some(false));
            },
            AppMsg::PreferencesUpdated(preferences) => {
                *self.get_preferences().borrow_mut() = preferences;
                self.set_preferences(self.get_preferences().clone());
            },
            AppMsg::DispatchInputEvent(InputEvent(source, event)) => {
                for slave in self.slaves.iter() {
                    let slave_model = slave.model().unwrap();
                    if let Some(target_input_source) = slave_model.get_input_source() {
                        if target_input_source.eq(&source) {
                            slave_model.input_event_sender.send(event.clone()).unwrap();
                        }
                    }
                }
            },
            AppMsg::ToggleRecording => match self.recording {
                Some(recording) => {
                    for (index, component) in self.slaves.iter().enumerate() {
                        let model = component.model().unwrap();
                        if recording {
                            model.get_video().send(SlaveVideoMsg::StopRecord).unwrap();
                        } else {
                            model.get_video().send(SlaveVideoMsg::StartRecord((index + 1).to_string())).unwrap();
                        }
                    }
                    self.set_recording(Some(!recording));
                },
                None => (),
            },
            AppMsg::SwitchColorScheme => {
                let style_manager = StyleManager::default();
                style_manager.set_color_scheme(if style_manager.is_dark() { ColorScheme::PreferLight } else { ColorScheme::ForceDark });
            },
            AppMsg::StopInputSystem => {
                self.input_system.stop();
            },
            AppMsg::DestroySlave(slave_ptr) => {
                if slave_ptr == std::ptr::null() {
                    self.slaves.pop();
                    return true;
                }
                let slave_index = self.slaves.iter().enumerate().find_map(move |(index, component)| if component.model().unwrap().deref() as *const SlaveModel == slave_ptr { Some(index)} else { None }).unwrap();
                if slave_index == self.slaves.len() - 1 {
                    self.slaves.pop();
                }
            },
        }
        // for i in 0..self.slaves.len() {
        //     self.get_mut_slaves().get_mut(i).unwrap().reset();
        // }
        true
    }
}


fn main() {
    gst::init().expect("无法初始化 GStreamer");
    gtk::init().map(|_| adw::init()).expect("无法初始化 GTK4");
    // sdl2::init().and_then(|sdl| init_joystick(&sdl)).expect("无法初始化 SDL2 用于手柄输入");
    let model = AppModel {
        ..Default::default()
    };
    model.input_system.run();
    
    let relm = RelmApp::new(model);
    // let win = ApplicationWindow::builder().build();
    // win.set_content(content)
    relm.run()
}
