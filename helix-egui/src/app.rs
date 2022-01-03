use egui::{Color32, Frame, Layout, Ui, Vec2, Widget};
use helix_core::{merge_toml_values, syntax};
use helix_lsp::{lsp, util::lsp_pos_to_pos, LspProgressMap};
use helix_view::{editor::Action, graphics::Rect, theme, Document, Editor, View};
use serde_json::json;

use anyhow::Result;

pub struct Application {
    editor: Editor,
}

impl Application {
    pub fn new() -> Result<Application> {
        let conf_dir = helix_core::config_dir();

        let theme_loader =
            std::sync::Arc::new(theme::Loader::new(&conf_dir, &helix_core::runtime_dir()));

        // load default and user config, and merge both
        let builtin_err_msg =
            "Could not parse built-in languages.toml, something must be very wrong";
        let def_lang_conf: toml::Value =
            toml::from_slice(include_bytes!("../../languages.toml")).expect(builtin_err_msg);
        let def_syn_loader_conf: helix_core::syntax::Configuration =
            def_lang_conf.clone().try_into().expect(builtin_err_msg);
        let user_lang_conf = std::fs::read(conf_dir.join("languages.toml"))
            .ok()
            .map(|raw| toml::from_slice(&raw));
        let lang_conf = match user_lang_conf {
            Some(Ok(value)) => Ok(merge_toml_values(def_lang_conf, value)),
            Some(err @ Err(_)) => err,
            None => Ok(def_lang_conf),
        };

        let syn_loader_conf: helix_core::syntax::Configuration = lang_conf
            .and_then(|conf| conf.try_into())
            .unwrap_or_else(|err| {
                eprintln!("Bad language config: {}", err);
                eprintln!("Press <ENTER> to continue with default language config");
                use std::io::Read;
                // This waits for an enter press.
                let _ = std::io::stdin().read(&mut []);
                def_syn_loader_conf
            });
        let syn_loader = std::sync::Arc::new(syntax::Loader::new(syn_loader_conf));

        let mut editor = Editor::new(
            Rect::new(0, 0, 100, 100), // Gets resized later
            theme_loader.clone(),
            syn_loader.clone(),
            Default::default(), // TODO: Grab editor config
        );
        let path = helix_core::runtime_dir().join("tutor.txt");
        editor.open(path, Action::VerticalSplit)?;
        editor.open("./src/main.rs".into(), Action::VerticalSplit)?;
        Ok(Application { editor })
    }

    pub fn render(self: &mut Application, ui: &mut Ui) {
        egui::CentralPanel::default().show(ui.ctx(), |ui| {
            ui.add(EditorWidget {
                editor: &mut self.editor,
            });
        });
    }
}

struct EditorWidget<'a> {
    editor: &'a mut Editor,
}

impl<'a> Widget for EditorWidget<'a> {
    fn ui(self, ui: &mut Ui) -> egui::Response {
        self.editor.resize(Rect::new(
            0,
            0,
            (ui.available_width() as f32 / ui.fonts().glyph_width(egui::TextStyle::Monospace, 'm'))
                .floor() as u16,
            (ui.available_height() as f32 / ui.fonts().row_height(egui::TextStyle::Monospace))
                .floor() as u16,
        ));
        ui.with_layout(Layout::left_to_right(), |ui| {
            for (view, focused) in self.editor.tree.views() {
                ui.add(ViewWidget {
                    view,
                    focused,
                    editor: self.editor,
                });
            }
        })
        .response
    }
}

struct ViewWidget<'a> {
    view: &'a View,
    focused: bool,
    editor: &'a Editor,
}

impl<'a> Widget for ViewWidget<'a> {
    fn ui(self, ui: &mut Ui) -> egui::Response {
        let doc = self.editor.document(self.view.doc).unwrap();
        ui.with_layout(Layout::top_down(egui::Align::Min), |ui| {
            if self.focused {
                Frame::default()
                    .stroke(egui::Stroke {
                        width: 1.,
                        color: Color32::WHITE,
                    })
                    .margin((4., 4.))
            } else {
                Frame::default()
            }
            .show(ui, |ui| {
                ui.allocate_ui(
                    (
                        self.view.inner_area().width as f32
                            * ui.fonts().glyph_width(egui::TextStyle::Monospace, 'm'),
                        self.view.inner_area().height as f32
                            * ui.fonts().row_height(egui::TextStyle::Monospace),
                    )
                        .into(),
                    |ui| {
                        ui.add(DocumentWidget { doc });
                    },
                );
                ui.with_layout(Layout::bottom_up(egui::Align::Min), |ui| {
                    ui.label(match doc.mode() {
                        helix_view::document::Mode::Normal => "NOR",
                        helix_view::document::Mode::Select => "SEL",
                        helix_view::document::Mode::Insert => "INS",
                    });
                });
            });
        })
        .response
    }
}

struct DocumentWidget<'a> {
    doc: &'a Document,
}

impl<'a> Widget for DocumentWidget<'a> {
    fn ui(self, ui: &mut Ui) -> egui::Response {
        for chunk in self.doc.text().chunks() {
            ui.label(chunk);
        }
        ui.allocate_response(ui.available_size(), egui::Sense::focusable_noninteractive())
    }
}
