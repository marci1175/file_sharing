use std::{cmp::Ordering, collections::HashMap, fmt::Display, fs, io::Write, path::PathBuf};

use chrono::{DateTime, Local};
use egui::{vec2, Color32, Context, RichText, Ui};
use egui_extras::{Column, TableBuilder};
use egui_notify::{Toast, Toasts};
use file_sharing::{
    client::{connect_to_server, ConnectionInstance, FileTree},
    server::start_server,
    DownloadHeader, Message,
};
use indexmap::IndexMap;
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio_util::sync::CancellationToken;
use tracing::{event, span, Level};

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct Application {
    pub file_trees: Vec<FileTree>,

    pub port_buf: String,

    pub remote_address: String,

    pub shared_folders: Vec<PathBuf>,

    #[serde(skip)]
    pub toasts: Toasts,

    #[serde(skip)]
    pub server_instance: Option<()>,

    #[serde(skip)]
    pub connection_instance: Option<ConnectionInstance>,

    pub shared_files: HashMap<String, PathBuf>,

    #[serde(skip)]
    pub server_cancellation_token: CancellationToken,

    #[serde(skip)]
    pub connection_reciver: Receiver<ConnectionInstance>,

    #[serde(skip)]
    pub connection_sender: Sender<ConnectionInstance>,

    #[serde(skip)]
    pub temporary_download_header_list: IndexMap<String, DownloadHeader>,

    pub completed_download_header_list: IndexMap<String, DownloadHeader>,

    #[serde(skip)]
    pub download_location: HashMap<String, PathBuf>,

    #[serde(skip)]
    client_cancellation_token: CancellationToken,
}

impl Default for Application {
    fn default() -> Self {
        let (connection_sender, connection_reciver) = channel(100);

        Self {
            client_cancellation_token: CancellationToken::new(),
            toasts: Toasts::new(),
            server_instance: None,
            connection_instance: None,
            file_trees: Vec::new(),
            port_buf: String::new(),
            shared_folders: Vec::new(),
            shared_files: HashMap::new(),
            server_cancellation_token: CancellationToken::new(),
            connection_reciver,
            connection_sender,
            remote_address: String::new(),
            temporary_download_header_list: IndexMap::new(),
            completed_download_header_list: IndexMap::new(),
            download_location: HashMap::new(),
        }
    }
}

impl Application {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        if let Some(storage) = cc.storage {
            return eframe::get_value(storage, eframe::APP_KEY).unwrap_or_default();
        }

        Default::default()
    }
}

impl eframe::App for Application {
    /// Called by the frame work to save state before shutdown.
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, self);
    }

    /// Called each time the UI needs repainting, which may be many times per second.
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.toasts.show(ctx);

        //Get the folder's item every repaint
        if self.server_instance.is_some() {
            if let Ok((file_trees, shared_files)) =
                folder_into_file_tree(self.shared_folders.clone())
            {
                if self.file_trees != file_trees {
                    self.file_trees = file_trees;
                    self.shared_files = shared_files;
                }
            }
        }

        //Create the top panel in the ui
        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            ui.horizontal(|ui| {
                //The "Host" menu button
                ui.menu_button("Host", |ui| {
                    ui.add_enabled_ui(self.server_instance.is_none(), |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Port");
                            ui.text_edit_singleline(&mut self.port_buf);
                        });

                        ui.label("Selected folders:");

                        ui.allocate_ui(vec2(200., 200.), |ui| {
                            egui::ScrollArea::new([true, true]).show(ui, |ui| {
                                for path in &self.shared_folders {
                                    ui.label(format!("{}", path.display()));
                                }
                            });
                        });

                        if ui.button("Select folder").clicked() {
                            if let Some(paths) = rfd::FileDialog::new().pick_folders() {
                                if let Ok((file_trees, shared_files)) =
                                    folder_into_file_tree(paths.clone())
                                {
                                    self.shared_folders = paths.clone();

                                    self.file_trees = file_trees;
                                    self.shared_files = shared_files;
                                }
                            }
                        }

                        let port_parse: Result<u16, std::num::ParseIntError> =
                            self.port_buf.parse();
                        let could_parse = port_parse.is_ok();

                        //Display Error
                        if !could_parse {
                            ui.label(
                                RichText::from("Invalid port!")
                                    .color(Color32::RED)
                                    .size(10.),
                            );
                        }

                        ui.add_enabled_ui(could_parse, |ui| {
                            if ui.button("Host").clicked() {
                                if let Err(err) = start_server(
                                    port_parse.unwrap(),
                                    self.server_cancellation_token.clone(),
                                    self.shared_files.clone(),
                                    self.file_trees.clone(),
                                ) {
                                    display_error(err);
                                } else {
                                    self.server_instance = Some(());
                                }
                            }
                        });
                    });
                    ui.add_enabled_ui(self.server_instance.is_some(), |ui| {
                        if ui
                            .button(RichText::from("Shutdown").color(Color32::RED))
                            .clicked()
                        {
                            self.server_cancellation_token.cancel();
                            self.server_instance = None;
                            self.server_cancellation_token = CancellationToken::new();
                        }
                    });
                });

                ui.menu_button("Connect", |ui| {
                    let connection_sender = self.connection_sender.clone();

                    ui.text_edit_singleline(&mut self.remote_address);

                    let remote_address = self.remote_address.clone();

                    let ctx = ctx.clone();
                    let client_cancellation_token = self.client_cancellation_token.clone();
                    ui.add_enabled_ui(self.connection_instance.is_none(), |ui| {
                        if ui.button("Connect").clicked() {
                            tokio::spawn(async move {
                                match connect_to_server(
                                    remote_address,
                                    ctx,
                                    client_cancellation_token,
                                )
                                .await
                                {
                                    Ok(connection_instance) => {
                                        connection_sender.send(connection_instance).await.unwrap();
                                    }
                                    Err(err) => {
                                        display_error(err);
                                    }
                                }
                            });
                        }
                    });

                    ui.add_enabled_ui(self.connection_instance.is_some(), |ui| {
                        if ui
                            .button(RichText::from("Disconnect").color(Color32::RED))
                            .clicked()
                        {
                            self.client_cancellation_token.cancel();
                            self.connection_instance = None;
                            self.client_cancellation_token = CancellationToken::new();
                        }
                    });
                });

                ui.menu_button("Download history", |ui| {
                    TableBuilder::new(ui)
                        .resizable(true)
                        .auto_shrink([true, false])
                        .striped(true)
                        .columns(Column::remainder().at_most(ctx.available_rect().width()), 5)
                        .header(25., |mut row| {
                            row.col(|ui| {
                                ui.label("Download Name");
                            });
                            row.col(|ui| {
                                ui.label("File Hash");
                            });
                            row.col(|ui| {
                                ui.label("Download size (Bytes)");
                            });
                            row.col(|ui| {
                                ui.label("Download packet count");
                            });
                            row.col(|ui| {
                                ui.label("Date");
                            });
                        })
                        .body(|body| {
                            let row_heights =
                                vec![25.; self.completed_download_header_list.len()].into_iter();

                            body.heterogeneous_rows(row_heights, |mut row| {
                                let row_idx = row.index();

                                let download_header =
                                    self.completed_download_header_list[row_idx].clone();

                                let file_response = download_header.file_response.clone();

                                row.col(|ui| {
                                    ui.label(file_response.file_name);
                                });
                                row.col(|ui| {
                                    ui.label(file_response.file_hash);
                                });
                                row.col(|ui| {
                                    ui.label(file_response.total_size.to_string());
                                });
                                row.col(|ui| {
                                    ui.label(file_response.file_packet_count.to_string());
                                });
                                row.col(|ui| {
                                    ui.label(download_header.initiated_stamp.to_rfc2822());
                                });
                            });
                        });
                });
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::both()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let clicked_file_hash = display_file_tree(ui, ctx, self.file_trees.clone(), self.temporary_download_header_list.clone());
                    if let Some(connection_instance) = &mut self.connection_instance {
                        if let Some((file_hash, file_name)) = clicked_file_hash {
                            let (file_name, file_extension) =
                                file_name.split_at(file_name.find('.').unwrap_or_default());
                            if let Some(save_location) = rfd::FileDialog::new()
                                .set_file_name(file_name)
                                .add_filter(file_extension, &[file_extension[1..].to_string()])
                                .save_file()
                            {
                                //backup temp path
                                self.download_location
                                    .insert(file_hash.clone(), save_location);

                                if let Err(err) =
                                    connection_instance.to_server_send.try_send(Message(Some(
                                        file_sharing::MessageType::FileRequest(file_hash),
                                    )))
                                {
                                    display_error(err);
                                }
                            }
                        }

                        if let Ok(Message(message)) =
                            connection_instance.from_server_recv.try_recv()
                        {
                            if let Some(message) = message {
                                match message {
                                    file_sharing::MessageType::FileResponse(file_reponse_header) => {
                                        self.temporary_download_header_list.insert(
                                            file_reponse_header.packet_identificator.clone(),
                                            DownloadHeader { file_response: file_reponse_header, packet_list: vec![], arrived_bytes: 0, initiated_stamp: Local::now() },
                                        );
                                    }
                                    file_sharing::MessageType::FilePacket(packet) => {
                                        let mut should_delete_row = false;

                                        let packet_id = packet.parent_id;

                                        let span = span!(Level::ERROR, "FilePacketReciver");
                                        let _enter = span.enter();
                                        
                                        if let Some(download_header) =
                                            self.temporary_download_header_list.get_mut(&packet_id)
                                        {
                                            download_header.packet_list.push(packet.packet_id);

                                            should_delete_row = download_header.file_response.file_packet_count as usize
                                                == download_header.packet_list.len();

                                            if should_delete_row {
                                                self.toasts.add(Toast::success(format!(
                                                    "{} has finished downloading.",
                                                    download_header.file_response.file_name
                                                )));
                                                self.completed_download_header_list.insert(packet_id.clone(), download_header.clone());
                                            }
    
                                            if let Some(save_path) =
                                                self.download_location.get(&packet.file_hash)
                                            {
                                                if let Ok(mut file) = fs::OpenOptions::new()
                                                    .create(true)
                                                    .append(true)
                                                    .open(save_path)
                                                {
                                                    download_header.arrived_bytes += file.write(&packet.bytes).unwrap();
                                                }
                                            }
                                        }
                                        else {
                                            event!(Level::ERROR, "Tried to receive a `FilePacket` without a `FileResponseHeader`.")
                                        }
                                        if should_delete_row {
                                            self.download_location.remove(&packet.file_hash);
                                            self.temporary_download_header_list.swap_remove(&packet_id);
                                        }
                                    }
                                    file_sharing::MessageType::KeepAlive => (),
                                    file_sharing::MessageType::FileTreeResponse(file_trees) => {
                                        self.file_trees = file_trees;
                                    }
                                    _ => unreachable!(),
                                }
                            }
                            else {
                                event!(Level::ERROR, "Received message empty `None` from server indicating an issue.");

                                if let Err(err) = connection_instance.to_server_send.try_send(Message(Some(file_sharing::MessageType::FileTreeRequest))) {
                                    display_error(err);
                                }
                            }
                        }
                    }
                });
        });

        if let Ok(incoming_connection) = self.connection_reciver.try_recv() {
            self.file_trees = incoming_connection.file_trees.clone();

            self.connection_instance = Some(incoming_connection);
        }
    }
}

fn display_error(err: impl ToString) {
    rfd::MessageDialog::new()
        .set_title("Error")
        .set_description(err.to_string())
        .show();
}

pub fn display_file_tree(
    ui: &mut Ui,
    ctx: &Context,
    file_trees: Vec<FileTree>,
    download_list: IndexMap<String, DownloadHeader>,
) -> Option<(String, String)> {
    for file_tree in file_trees {
        match &file_tree {
            FileTree::Folder((name, file_list)) => {
                if let Some(Some(file_attr)) = ui
                    .collapsing(name, |ui| {
                        for entry in file_list {
                            if let Some(file_attr) = display_file_tree(
                                ui,
                                ctx,
                                vec![entry.clone()],
                                download_list.clone(),
                            ) {
                                return Some(file_attr);
                            }
                        }
                        None
                    })
                    .body_returned
                {
                    return Some(file_attr);
                }
            }
            FileTree::File((file_name, file_hash)) => {
                return ui
                    .horizontal(|ui| {
                        if ui.button(file_name).clicked() {
                            return Some((file_hash.to_string(), file_name.to_string()));
                        };

                        if let Some(download_header) = download_list
                            .values()
                            .find(|entry| entry.file_response.file_hash == *file_hash)
                        {
                            ui.allocate_ui(vec2(200., ui.available_height()), |ui| {
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::ProgressBar::new(
                                            download_header.packet_list.len() as f32
                                                / download_header.file_response.file_packet_count
                                                    as f32,
                                        )
                                        .show_percentage()
                                        .text(
                                            calculate_bandwidth_from_date_and_bytes(
                                                download_header.arrived_bytes,
                                                download_header.initiated_stamp,
                                            )
                                            .to_string(),
                                        ),
                                    );

                                    ui.label(format!(
                                        "Elapsed {}s",
                                        Local::now()
                                            .signed_duration_since(download_header.initiated_stamp)
                                            .num_seconds()
                                    ));
                                });

                                ctx.request_repaint();
                            });
                        }

                        None
                    })
                    .inner;
            }
            FileTree::Empty => {
                ui.label("Empty");
            }
        };
    }

    None
}

pub fn folder_into_file_tree(
    paths: Vec<PathBuf>,
) -> anyhow::Result<(Vec<FileTree>, HashMap<String, PathBuf>)> {
    let mut shared_files: HashMap<String, PathBuf> = HashMap::new();
    let mut file_trees: Vec<FileTree> = vec![];

    for path in paths {
        let dir = fs::read_dir(&path)?;

        let mut file_tree_raw: (String, Vec<FileTree>) = (
            path.file_name()
                .ok_or_else(|| anyhow::Error::msg("File name doesnt exist"))?
                .to_string_lossy()
                .to_string(),
            vec![],
        );

        let dir_iter = dir.into_iter();

        for entry in dir_iter {
            let entry = entry?;

            let entry_path = entry.path();
            let file_full_name = entry_path
                .file_name()
                .ok_or_else(|| anyhow::Error::msg("File name doesnt exist"))?
                .to_string_lossy();

            if entry.file_type()?.is_dir() {
                let (file_tree, folder_shared_files) = folder_into_file_tree(vec![entry.path()])?;

                //Push token
                file_tree_raw.1.push(file_tree[0].clone());

                //Extend hashmap
                shared_files.extend(folder_shared_files.into_iter());
            } else {
                let hashed_path_as_string =
                    sha256::digest(entry_path.to_string_lossy().to_string());

                file_tree_raw.1.push(FileTree::File((
                    file_full_name.to_string(),
                    hashed_path_as_string.clone(),
                )));

                shared_files.insert(hashed_path_as_string, entry_path);
            }
        }

        file_trees.push(FileTree::Folder(file_tree_raw));
    }

    Ok((file_trees, shared_files))
}

pub fn calculate_bandwidth_from_date_and_bytes(bytes: usize, date: DateTime<Local>) -> Bandwidth {
    let bytes_per_sec = bytes
        / (Local::now().signed_duration_since(date).num_seconds() as usize).clamp(1, usize::MAX);
    let kbytes_per_sec = bytes_per_sec as f32 / 1024.;
    let mbytes_per_sec = kbytes_per_sec as f32 / 1024.;
    let mbits_per_sec = mbytes_per_sec * 8.;

    if mbits_per_sec > 1024. {
        let gbytes_per_sec = mbytes_per_sec as f32 / 1024.;

        let gbits_per_sec = gbytes_per_sec * 8.;

        Bandwidth::GBPS(gbits_per_sec)
    } else if mbits_per_sec < 1. {
        let kbits_per_sec = kbytes_per_sec * 8.;

        Bandwidth::KBPS(kbits_per_sec)
    } else {
        Bandwidth::MBPS(mbits_per_sec)
    }
}

pub enum Bandwidth {
    GBPS(f32),
    MBPS(f32),
    KBPS(f32),
}

impl Display for Bandwidth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&match self {
            Bandwidth::GBPS(num) => {
                format!("{num} GBPS")
            }
            Bandwidth::MBPS(num) => {
                format!("{num} MBPS")
            }
            Bandwidth::KBPS(num) => {
                format!("{num} KBPS")
            }
        })
    }
}
