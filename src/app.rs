use std::{collections::HashMap, fs, io::Write, path::PathBuf, thread};

use egui::{vec2, Color32, RichText, Ui};
use file_sharing::{
    client::{connect_to_server, ConnectionInstance, FileTree},
    server::start_server,
    FileReponseHeader, Message,
};
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct Application {
    #[serde(skip)]
    pub server_instance: Option<()>,
    #[serde(skip)]
    pub connection_instance: Option<ConnectionInstance>,

    pub file_trees: Vec<FileTree>,

    pub port_buf: String,
    pub remote_address: String,

    pub shared_folders: Vec<PathBuf>,

    pub shared_files: HashMap<String, PathBuf>,

    #[serde(skip)]
    pub server_cancellation_token: CancellationToken,

    #[serde(skip)]
    pub connection_reciver: Receiver<ConnectionInstance>,

    #[serde(skip)]
    pub connection_sender: Sender<ConnectionInstance>,

    pub download_header_list: HashMap<String, (FileReponseHeader, Vec<String>)>,
}

impl Default for Application {
    fn default() -> Self {
        let (connection_sender, connection_reciver) = channel(100);
        Self {
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
            download_header_list: HashMap::new(),
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
        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.add_enabled_ui(self.connection_instance.is_none(), |ui| {
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
                            }
                        });
                    });
                });

                ui.menu_button("Connect", |ui| {
                    let connection_sender = self.connection_sender.clone();

                    ui.text_edit_singleline(&mut self.remote_address);

                    let remote_address = self.remote_address.clone();

                    ui.add_enabled_ui(self.connection_instance.is_none(), |ui| {
                        if ui.button("Connect").clicked() {
                            tokio::spawn(async move {
                                match connect_to_server(remote_address).await {
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
                });
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::both()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if let Some(file_hash) = display_file_tree(ui, self.file_trees.clone()) {
                        if let Some(connection_instance) = &mut self.connection_instance {
                            if let Err(err) = connection_instance.to_server_send.try_send(Message(
                                Some(file_sharing::MessageType::FileRequest(file_hash)),
                            )) {
                                display_error(err);
                            }

                            if let Ok(Message(Some(message))) =
                                connection_instance.from_server_recv.try_recv()
                            {
                                match message {
                                    file_sharing::MessageType::FileResponse(
                                        file_reponse_header,
                                    ) => {
                                        self.download_header_list.insert(
                                            file_reponse_header.uuid.clone(),
                                            (file_reponse_header, vec![]),
                                        );
                                    }
                                    file_sharing::MessageType::FilePacket(packet) => {
                                        let mut should_delete_row = false;

                                        let packet_id = packet.parent_id;

                                        if let Some((_header, packet_hash_list)) =
                                            self.download_header_list.get_mut(&packet_id)
                                        {
                                            should_delete_row =
                                                _header.file_packet_count == packet_hash_list.len();

                                            packet_hash_list.push(packet.packet_id);

                                            dbg!(packet_hash_list.len());

                                            if let Ok(mut file) = fs::OpenOptions::new()
                                                .create(true)
                                                .append(true)
                                                .open(_header.uuid.to_string())
                                            {
                                                file.write(&packet.bytes).unwrap();
                                            }
                                        }

                                        if should_delete_row {
                                            self.download_header_list.remove(&packet_id);
                                        }
                                    }
                                    file_sharing::MessageType::KeepAlive => (),

                                    _ => unreachable!(),
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

pub fn display_file_tree(ui: &mut Ui, file_trees: Vec<FileTree>) -> Option<String> {
    for file_tree in file_trees {
        match &file_tree {
            FileTree::Folder((name, file_list)) => {
                if let Some(Some(file_hash)) = ui
                    .collapsing(name, |ui| {
                        for entry in file_list {
                            match display_file_tree(ui, vec![entry.clone()]) {
                                //If it's `Some()` it means a button has been pushed and we can return the value
                                Some(file_hash) => {
                                    return Some(file_hash);
                                }
                                //If it is none we dont want to return to iter over the other elements
                                None => (),
                            }
                        }
                        None
                    })
                    .body_returned
                {
                    return Some(file_hash);
                }
            }
            FileTree::File((file_name, file_hash)) => {
                if ui.button(file_name).clicked() {
                    return Some(file_hash.to_string());
                };
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
