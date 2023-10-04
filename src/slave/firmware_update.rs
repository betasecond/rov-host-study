/* firmware_update.rs
 *
 * Copyright 2021-2022 Bohong Huang
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program. If not, see <http://www.gnu.org/licenses/>.
 */

use std::error::Error;
use std::fmt::Display;
use std::{path::PathBuf, fmt::Debug};
use async_std::{io::ReadExt, task};

use glib::Sender;
use glib_macros::clone;
use gtk::{Align, Box as GtkBox, Orientation, prelude::*, FileFilter, ProgressBar, FileChooserAction, Button};
use adw::{HeaderBar, PreferencesGroup, StatusPage, Window, prelude::*, ActionRow, Carousel};
use once_cell::unsync::OnceCell;
use relm4::{send, MicroWidgets, MicroModel};
use relm4_macros::micro_widget;

use derivative::*;

use jsonrpsee_core::client::ClientT;

use crate::prelude::*;
use crate::slave::{SlaveCommunicationMsg, RpcClient, AsRpcParams, protocol::*};
use crate::ui::generic::select_path;

use super::SlaveMsg;

pub enum SlaveFirmwareUpdaterMsg {
    StartUpload,
    NextStep,
    FirmwareFileSelected(PathBuf),
    FirmwareUploadProgressUpdated(f32),
    FirmwareUploadFailed(SlaveFirmwareUpdateError),
}

#[tracker::track()]
#[derive(Debug, Derivative)]
#[derivative(Default)]
pub struct SlaveFirmwareUpdaterModel {
    current_page: u32,
    firmware_file_path: Option<PathBuf>,
    firmware_uploading_progress: f32,
    #[no_eq]
    _rpc_client: OnceCell<RpcClient>,
    #[no_eq]
    #[derivative(Default(value="Ok(())"))]
    firmware_update_result: Result<(), SlaveFirmwareUpdateError>,
}

impl SlaveFirmwareUpdaterModel {
    fn is_uploading(&self) -> bool {
        self.firmware_uploading_progress > 0.0 && self.firmware_uploading_progress < 1.0 && self.firmware_update_result.is_ok()
    }
}

#[derive(Debug)]
pub enum SlaveFirmwareUpdateError {
    IOError(std::io::Error),
    RpcError(jsonrpsee_core::Error),
    VerificationError(usize, usize),
}

impl Display for SlaveFirmwareUpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SlaveFirmwareUpdateError::IOError(error) => Display::fmt(error, f),
            SlaveFirmwareUpdateError::RpcError(error) => Display::fmt(error, f),
            SlaveFirmwareUpdateError::VerificationError(expect, given) => write!(f, "Verification error: The returned length {} doesn't match the expected {}", given, expect),
        }
    }
}

impl Error for SlaveFirmwareUpdateError {}

impl SlaveFirmwareUpdaterModel {
    pub fn new(rpc_client: RpcClient) -> SlaveFirmwareUpdaterModel {
        SlaveFirmwareUpdaterModel {
            _rpc_client: OnceCell::from(rpc_client),
            ..Default::default()
        }
    }
    
    pub fn get_rpc_client(&self) -> &RpcClient {
        self._rpc_client.get().unwrap()
    }
}

impl MicroModel for SlaveFirmwareUpdaterModel {
    type Msg = SlaveFirmwareUpdaterMsg;
    type Widgets = SlaveFirmwareUpdaterWidgets;
    type Data = Sender<SlaveMsg>;
    
    fn update(&mut self, msg: SlaveFirmwareUpdaterMsg, parent_sender: &Sender<SlaveMsg>, sender: Sender<SlaveFirmwareUpdaterMsg>) {
        self.reset();
        match msg {
            SlaveFirmwareUpdaterMsg::NextStep => self.set_current_page(self.get_current_page().wrapping_add(1)),
            SlaveFirmwareUpdaterMsg::FirmwareFileSelected(path) => self.set_firmware_file_path(Some(path)),
            SlaveFirmwareUpdaterMsg::FirmwareUploadProgressUpdated(progress) => {
                self.set_firmware_uploading_progress(progress);
                if progress >= 1.0 || progress < 0.0 {
                    send!(sender, SlaveFirmwareUpdaterMsg::NextStep);
                }
            },
            SlaveFirmwareUpdaterMsg::StartUpload => {
                if let Some(path) = self.get_firmware_file_path() {
                    const CHUNK_SIZE: usize = 1024;
                    send!(sender, SlaveFirmwareUpdaterMsg::NextStep);
                    let rpc_client = self.get_rpc_client().clone();
                    let handle = task::spawn(clone!(@strong sender, @strong path => async move {
                        match async_std::fs::File::open(path).await {
                            Ok(mut file) => {
                                let mut bytes = Vec::new();
                                file.read_to_end(&mut bytes).await.map_err(SlaveFirmwareUpdateError::IOError)?;
                                let len_total = bytes.len();
                                let mut len_remain = len_total;
                                for chunk in bytes.chunks(CHUNK_SIZE) {
                                    let chunk_encoded = base64::encode(chunk);
                                    match rpc_client.request::<usize>(METHOD_UPDATE_FIRMWARE, Some((chunk_encoded, len_remain).to_rpc_params())).await {
                                        Ok(len_received) => {
                                            if len_received == chunk.len() {
                                                len_remain -= len_received;
                                                send!(sender, SlaveFirmwareUpdaterMsg::FirmwareUploadProgressUpdated((len_total - len_remain) as f32 / len_total as f32))
                                            } else {
                                                return Err(SlaveFirmwareUpdateError::VerificationError(chunk.len(), len_received));
                                            }
                                        },
                                        Err(err) => {
                                            return Err(SlaveFirmwareUpdateError::RpcError(err));
                                        },
                                    }
                                }
                                Ok(())
                            },
                            Err(err) => Err(SlaveFirmwareUpdateError::IOError(err)),
                        }
                    }));
                    let handle = task::spawn(async move {
                        let result = handle.await;
                        if let Err(err) = result {
                            send!(sender, SlaveFirmwareUpdaterMsg::FirmwareUploadFailed(err));
                        }
                        Ok(())
                    });
                    send!(parent_sender, SlaveMsg::CommunicationMessage(SlaveCommunicationMsg::Block(handle)));
                }
            },
            SlaveFirmwareUpdaterMsg::FirmwareUploadFailed(err) => {
                self.set_firmware_update_result(Err(err));
                send!(sender, SlaveFirmwareUpdaterMsg::NextStep);
            },
        }
    }
}

#[micro_widget(pub)]
impl MicroWidgets<SlaveFirmwareUpdaterModel> for SlaveFirmwareUpdaterWidgets {
    view! {
        window = Window {
            set_title: Some("固件更新向导"),
            set_width_request: 480,
            set_height_request: 480,
            set_destroy_with_parent: true,
            set_modal: true,
            set_content = Some(&GtkBox) {
                set_orientation: Orientation::Vertical,
                append = &HeaderBar {
                    set_sensitive: track!(model.changed(SlaveFirmwareUpdaterModel::firmware_uploading_progress()) || model.changed(SlaveFirmwareUpdaterModel::firmware_update_result()), !model.is_uploading()),
                },
                append: carousel = &Carousel {
                    set_hexpand: true,
                    set_vexpand: true,
                    set_interactive: false,
                    scroll_to_page: track!(model.changed(SlaveFirmwareUpdaterModel::current_page()), model.current_page, true),
                    append = &StatusPage {
                        set_icon_name: Some("software-update-available-symbolic"),
                        set_title: "欢迎使用固件更新向导",
                        set_hexpand: true,
                        set_vexpand: true,
                        set_description: Some("请确保固件更新期间机器人有充足的电量供应。"),
                        set_child = Some(&Button) {
                            set_css_classes: &["suggested-action", "pill"],
                            set_halign: Align::Center,
                            set_label: "下一步",
                            connect_clicked(sender) => move |_button| {
                                send!(sender, SlaveFirmwareUpdaterMsg::NextStep);
                            },
                        },
                    },
                    append = &StatusPage {
                        set_icon_name: Some("folder-open-symbolic"),
                        set_title: "请选择固件文件",
                        set_hexpand: true,
                        set_vexpand: true,
                        set_description: Some("选择的固件文件必须为下位机的可执行文件。"),
                        set_child = Some(&GtkBox) {
                            set_orientation: Orientation::Vertical,
                            set_spacing: 50,
                            append = &PreferencesGroup {
                                add = &ActionRow {
                                    set_title: "固件文件",
                                    set_subtitle: track!(model.changed(SlaveFirmwareUpdaterModel::firmware_file_path()), &model.firmware_file_path.as_ref().map_or("请选择文件".to_string(), |path| path.to_str().unwrap().to_string())),
                                    add_suffix: browse_firmware_file_button = &Button {
                                        set_label: "浏览",
                                        set_valign: Align::Center,
                                        connect_clicked(sender, window) => move |_button| {
                                            let filter = FileFilter::new();
                                            filter.add_suffix("tar.gz");
                                            filter.set_name(Some("固件文件"));
                                            std::mem::forget(select_path(FileChooserAction::Open, &[filter], &window, clone!(@strong sender => move |path| {
                                                match path {
                                                    Some(path) => {
                                                        send!(sender, SlaveFirmwareUpdaterMsg::FirmwareFileSelected(path));
                                                    },
                                                    None => (),
                                                }
                                            }))); // 内存泄露修复
                                        },
                                    },
                                    set_activatable_widget: Some(&browse_firmware_file_button),
                                },
                            },
                            append = &Button {
                                set_css_classes: &["suggested-action", "pill"],
                                set_halign: Align::Center,
                                set_label: "开始更新",
                                set_sensitive: track!(model.changed(SlaveFirmwareUpdaterModel::firmware_file_path()), model.get_firmware_file_path().as_ref().map_or(false, |pathbuf| pathbuf.exists() && pathbuf.is_file())),
                                connect_clicked(sender) => move |_button| {
                                    send!(sender, SlaveFirmwareUpdaterMsg::StartUpload);
                                },
                            }
                        },
                    },
                    append = &StatusPage {
                        set_icon_name: Some("folder-download-symbolic"),
                        set_title: "正在更新固件...",
                        set_hexpand: true,
                        set_vexpand: true,
                        set_description: Some("请不要切断连接或电源。"),
                        set_child = Some(&GtkBox) {
                            set_orientation: Orientation::Vertical,
                            set_spacing: 50,
                            append = &ProgressBar {
                                set_fraction: track!(model.changed(SlaveFirmwareUpdaterModel::firmware_uploading_progress()), *model.get_firmware_uploading_progress() as f64)
                            },
                        },
                    },
                    append = &StatusPage {
                        set_icon_name: track!(model.changed(SlaveFirmwareUpdaterModel::firmware_update_result()), if model.get_firmware_update_result().is_ok() { Some("emblem-ok-symbolic") } else { Some("dialog-warning-symbolic") }),
                        set_title: track!(model.changed(SlaveFirmwareUpdaterModel::firmware_update_result()), if model.get_firmware_update_result().is_ok() { "固件更新成功" } else { "固件更新失败" }),
                        set_hexpand: true,
                        set_vexpand: true,
                        set_description: track!(model.changed(SlaveFirmwareUpdaterModel::firmware_update_result()), Some(
                            match model.get_firmware_update_result() {
                                Ok(_) => "机器人将自动重启，请稍后手动进行连接。".to_string(),
                                Err(err) => format!("请检查文件与网络连接是否正常。\n\n{}", err)}).as_deref()),
                        set_child = Some(&Button) {
                            set_css_classes: &["suggested-action", "pill"],
                            set_halign: Align::Center,
                            set_label: "完成",
                            connect_clicked(window) => move |_button| {
                                window.destroy();
                            },
                        },
                    },
                },
            },
        }
    }
}

impl Debug for SlaveFirmwareUpdaterWidgets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(&self.root_widget(), f)
    }
}
