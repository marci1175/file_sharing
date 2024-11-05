use std::path::PathBuf;

use egui::{vec2, Response, Ui};
use file_sharing::{
    client::{ConnectionInstance, FileTree},
    server::ServerInstance,
};

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct Application {
    #[serde(skip)]
    pub server_instance: Option<ServerInstance>,
    #[serde(skip)]
    pub connection_instance: Option<ConnectionInstance>,

    pub file_tree: FileTree,

    pub port_buf: String,

    pub shared_folders: Vec<PathBuf>,
}

impl Default for Application {
    fn default() -> Self {
        Self {
            server_instance: None,
            connection_instance: None,
            file_tree: FileTree::Empty,
            port_buf: String::new(),
            shared_folders: Vec::new(),
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
                ui.add_enabled_ui(
                    self.server_instance.is_none() || self.connection_instance.is_none(),
                    |ui| {
                        ui.menu_button("Host", |ui| {
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
                                    self.shared_folders = paths;
                                }
                            }

                            if ui.button("Host").clicked() {}
                        });
                    },
                );

                ui.add_enabled_ui(
                    self.server_instance.is_none() || self.connection_instance.is_none(),
                    |ui| {
                        if ui.button("Connect").clicked() {}
                    },
                );
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            display_file_tree(ui, self.file_tree.clone());
        });
    }
}

pub fn display_file_tree(ui: &mut Ui, file_tree: FileTree)
{
    match &file_tree {
        FileTree::Folder((name, file_list)) => {
            ui.collapsing(name, |ui| {
                for entry in file_list {
                    display_file_tree(ui, entry.clone())
                }
            });
        },
        FileTree::File((file_name, file_hash)) => {
            if ui.button(file_name).clicked() {
                println!("{file_hash}")
            };
        },
        FileTree::Empty => {
            ui.label("Empty");
        },
    };
}

pub fn folder_into_file_tree(path: PathBuf) -> FileTree {
    FileTree::Empty
}