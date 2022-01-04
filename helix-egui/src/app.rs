use egui::{Color32, Frame, Layout, Pos2, Ui, Vec2, Widget};
use helix_core::{
    graphemes::ensure_grapheme_boundary_next,
    merge_toml_values,
    syntax::{self, Highlight, HighlightEvent, Loader},
    LineEnding, Position,
};
use helix_view::{editor::Action, graphics::Rect, theme, Document, Editor, Theme, View};

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

        let theme = theme_loader
            .load("nord")
            .map_err(|e| {
                log::warn!("failed to load theme `{}` - {}", "nord", e);
                e
            })
            .ok();

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
        if let Some(theme) = theme {
            editor.set_theme(theme);
        }
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
                        self.view.area.width as f32
                            * ui.fonts().glyph_width(egui::TextStyle::Monospace, 'm'),
                        self.view.area.height as f32
                            * ui.fonts().row_height(egui::TextStyle::Monospace),
                    )
                        .into(),
                    |ui| {
                        ui.add(DocumentWidget {
                            doc,
                            offset: self.view.offset,
                            area: self.view.inner_area(),
                            loader: &self.editor.syn_loader,
                            theme: &self.editor.theme,
                        });
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
    offset: Position,
    area: Rect,
    loader: &'a Loader,
    theme: &'a Theme,
}

fn dumb_log(num: u16) -> u16 {
    match num {
        0..=9 => 1,
        10..=99 => 2,
        100..=999 => 3,
        1000..=9999 => 4,
        _ => unreachable!(), // TODO: make this not suck :)
    }
}

impl<'a> DocumentWidget<'a> {
    fn build_highlights(&'a self) -> impl Iterator<Item = HighlightEvent> + 'a {
        let Self {
            doc,
            loader,
            theme,
            offset,
            ..
        } = self;
        let text = doc.text().slice(..);
        let last_line = std::cmp::min(
            // Saturating subs to make it inclusive zero indexing.
            (offset.row + self.area.height as usize).saturating_sub(1),
            doc.text().len_lines().saturating_sub(1),
        );

        let range = {
            // calculate viewport byte ranges
            let start = text.line_to_byte(offset.row);
            let end = text.line_to_byte(last_line + 1);

            start..end
        };

        // TODO: range doesn't actually restrict source, just highlight range
        match doc.syntax() {
            Some(syntax) => {
                let scopes = theme.scopes();
                syntax
                    .highlight_iter(text.slice(..), Some(range), None, |language| {
                        loader.language_configuration_for_injection_string(language)
                            .and_then(|language_config| {
                                let config = language_config.highlight_config(scopes)?;
                                let config_ref = config.as_ref();
                                // SAFETY: the referenced `HighlightConfiguration` behind
                                // the `Arc` is guaranteed to remain valid throughout the
                                // duration of the highlight.
                                let config_ref = unsafe {
                                    std::mem::transmute::<
                                        _,
                                        &'static syntax::HighlightConfiguration,
                                    >(config_ref)
                                };
                                Some(config_ref)
                            })
                    })
                    .map(|event| event.unwrap())
                    .collect() // TODO: we collect here to avoid holding the lock, fix later
            }
            None => vec![HighlightEvent::Source {
                start: range.start,
                end: range.end,
            }],
        }
        .into_iter()
        .map(move |event| match event {
            // convert byte offsets to char offset
            HighlightEvent::Source { start, end } => {
                let start = ensure_grapheme_boundary_next(text, text.byte_to_char(start));
                let end = ensure_grapheme_boundary_next(text, text.byte_to_char(end));
                HighlightEvent::Source { start, end }
            }
            event => event,
        })
    }
}

fn get_grapheme_index(val: &str, index: usize) -> usize {
    val.char_indices()
        .map(|(i, c)| i + c.len_utf8())
        .take_while(|i| *i <= index)
        .last()
        .unwrap_or(0)
}

fn convert_color(color: helix_view::graphics::Color) -> Color32 {
    match color {
        helix_view::graphics::Color::Reset => todo!(),
        helix_view::graphics::Color::Black => Color32::BLACK,
        helix_view::graphics::Color::Red => Color32::RED,
        helix_view::graphics::Color::Green => Color32::GREEN,
        helix_view::graphics::Color::Yellow => Color32::YELLOW,
        helix_view::graphics::Color::Blue => Color32::BLUE,
        helix_view::graphics::Color::Magenta => Color32::from_rgb(255, 0, 255),
        helix_view::graphics::Color::Cyan => Color32::from_rgb(0, 255, 255),
        helix_view::graphics::Color::Gray => Color32::GRAY,
        helix_view::graphics::Color::LightRed => Color32::LIGHT_RED,
        helix_view::graphics::Color::LightGreen => Color32::LIGHT_GREEN,
        helix_view::graphics::Color::LightYellow => Color32::LIGHT_YELLOW,
        helix_view::graphics::Color::LightBlue => Color32::LIGHT_BLUE,
        helix_view::graphics::Color::LightMagenta => Color32::from_rgb(255, 128, 255),
        helix_view::graphics::Color::LightCyan => Color32::from_rgb(128, 255, 255),
        helix_view::graphics::Color::LightGray => Color32::LIGHT_GRAY,
        helix_view::graphics::Color::White => Color32::WHITE,
        helix_view::graphics::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
        helix_view::graphics::Color::Indexed(_) => todo!(),
    }
}

impl<'a> Widget for DocumentWidget<'a> {
    fn ui(self, ui: &mut Ui) -> egui::Response {
        let Self {
            theme,
            doc,
            area,
            offset,
            ..
        } = self;
        let line_height = ui.fonts().row_height(egui::TextStyle::Monospace);
        let char_width = ui.fonts().glyph_width(egui::TextStyle::Monospace, 'm');
        let highlights = self.build_highlights();
        let available_rect = ui.available_rect_before_wrap();
        let top_left = available_rect.left_top();
        let mut paint_cursor = top_left;
        let text_style = theme.get("ui.text");
        let mut spans: Vec<Highlight> = Vec::new();
        let text = doc.text().slice(..);

        let mut visual_x = 0u16;
        let mut line = 1u16;
        let painter = ui.painter();
        // Render gutter
        paint_cursor += Vec2::RIGHT * char_width * (5 - dumb_log(line + area.y)) as f32;

        painter.text(
            paint_cursor,
            egui::Align2::LEFT_TOP,
            line + area.y,
            egui::TextStyle::Monospace,
            theme
                .get("ui.linenr")
                .fg
                .map(convert_color)
                .unwrap_or(Color32::WHITE),
        );
        paint_cursor += Vec2::RIGHT * char_width * (dumb_log(line + area.y) + 1) as f32;

        'outer: for highlight in highlights {
            match highlight {
                HighlightEvent::Source { start, end } => {
                    let text = text.get_slice(start..end).unwrap_or_else(|| " ".into());

                    for chunk_line in text.chunks().map(|c| c.split_inclusive('\n')).flatten() {
                        if visual_x < area.width {
                            let trimmed = {
                                let mut val = chunk_line.trim_end_matches('\n');
                                if val.len() as u16 + visual_x >= area.width {
                                    val = &val[0..get_grapheme_index(
                                        val,
                                        (area.width - visual_x) as usize,
                                    )];
                                }
                                if visual_x < offset.row as u16 {
                                    if val.len() > offset.row {
                                        visual_x = offset.row as u16;
                                        val = &val[get_grapheme_index(val, offset.col)..];
                                    } else {
                                        visual_x += val.len() as u16;
                                        continue; // This is hacky, find a better way
                                    }
                                };
                                val
                            };
                            if !trimmed.is_empty() && visual_x < area.width {
                                let style = spans.iter().fold(text_style, |acc, span| {
                                    acc.patch(theme.highlight(span.0))
                                });
                                let res = painter.text(
                                    paint_cursor,
                                    egui::Align2::LEFT_TOP,
                                    trimmed,
                                    egui::TextStyle::Monospace,
                                    style.fg.map(convert_color).unwrap_or(Color32::WHITE),
                                );
                                paint_cursor += Vec2::RIGHT * res.width();

                                // There's probably some graphene stuff I'm botching here
                                visual_x = visual_x.saturating_add(chunk_line.len() as u16);
                            }
                        }
                        if chunk_line.ends_with('\n') {
                            paint_cursor = Pos2 {
                                x: top_left.x,
                                y: paint_cursor.y + line_height,
                            };
                            visual_x = 0;
                            line += 1;
                            if line > area.height {
                                break 'outer; // short-circuit if we're going to pass the end of the screen
                            }
                            let line_number = area.y + line;

                            // Render gutter
                            paint_cursor +=
                                Vec2::RIGHT * char_width * (5 - dumb_log(line_number)) as f32;

                            painter.text(
                                paint_cursor,
                                egui::Align2::LEFT_TOP,
                                line_number,
                                egui::TextStyle::Monospace,
                                theme
                                    .get("ui.linenr")
                                    .fg
                                    .map(convert_color)
                                    .unwrap_or(Color32::WHITE),
                            );
                            paint_cursor +=
                                Vec2::RIGHT * char_width * (dumb_log(line_number) + 1) as f32;
                        }
                    }
                }
                HighlightEvent::HighlightStart(highlight) => {
                    spans.push(highlight);
                }
                HighlightEvent::HighlightEnd => {
                    spans.pop();
                }
            }
        }
        ui.allocate_response(ui.available_size(), egui::Sense::focusable_noninteractive())
    }
}
