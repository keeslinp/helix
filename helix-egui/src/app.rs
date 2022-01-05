use egui::{Color32, CtxRef, Frame, Layout, Pos2, Ui, Vec2, Widget};
use helix_core::{
    graphemes::{ensure_grapheme_boundary_next, next_grapheme_boundary, prev_grapheme_boundary},
    merge_toml_values,
    syntax::{self, Highlight, HighlightEvent, Loader},
    LineEnding, Position,
};
use helix_view::{
    document::Mode,
    editor::Action,
    graphics::{Modifier, Rect},
    theme, Document, Editor, Theme, View,
};

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
        egui::CentralPanel::default()
            .frame(
                Frame::default().fill(
                    self.editor
                        .theme
                        .get("ui.background")
                        .bg
                        .map(convert_color)
                        .unwrap_or(Color32::TRANSPARENT),
                ),
            )
            .show(ui.ctx(), |ui| {
                ui.add(EditorWidget {
                    editor: &mut self.editor,
                });
            });
    }

    pub fn resize(self: &mut Application, width: u32, height: u32, ctx: CtxRef) {
        self.editor.resize(Rect::new(
            0,
            0,
            (width as f32 / ctx.fonts().glyph_width(egui::TextStyle::Monospace, 'm')).floor()
                as u16,
            (height as f32 / ctx.fonts().row_height(egui::TextStyle::Monospace)).floor() as u16,
        ));
    }
}

struct EditorWidget<'a> {
    editor: &'a mut Editor,
}

impl<'a> Widget for EditorWidget<'a> {
    fn ui(self, ui: &mut Ui) -> egui::Response {
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

impl<'a> ViewWidget<'a> {
    fn build_highlights(&'a self) -> Box<dyn Iterator<Item = HighlightEvent> + 'a> {
        if self.focused {
            Box::new(syntax::merge(
                self.build_syntax_highlights(),
                self.build_selection_highlights(),
            ))
        } else {
            Box::new(self.build_syntax_highlights())
        }
    }
    fn build_selection_highlights(&'a self) -> Vec<(usize, std::ops::Range<usize>)> {
        let doc = self.editor.document(self.view.doc).unwrap();
        let theme = &self.editor.theme;
        let text = doc.text().slice(..);
        let selection = doc.selection(self.view.id);
        let primary_idx = selection.primary_index();

        let selection_scope = theme
            .find_scope_index("ui.selection")
            .expect("could not find `ui.selection` scope in the theme!");
        let base_cursor_scope = theme
            .find_scope_index("ui.cursor")
            .unwrap_or(selection_scope);

        let cursor_scope = match doc.mode() {
            Mode::Insert => theme.find_scope_index("ui.cursor.insert"),
            Mode::Select => theme.find_scope_index("ui.cursor.select"),
            Mode::Normal => Some(base_cursor_scope),
        }
        .unwrap_or(base_cursor_scope);

        let primary_cursor_scope = theme
            .find_scope_index("ui.cursor.primary")
            .unwrap_or(cursor_scope);
        let primary_selection_scope = theme
            .find_scope_index("ui.selection.primary")
            .unwrap_or(selection_scope);

        let mut spans: Vec<(usize, std::ops::Range<usize>)> = Vec::new();
        for (i, range) in selection.iter().enumerate() {
            let (cursor_scope, selection_scope) = if i == primary_idx {
                (primary_cursor_scope, primary_selection_scope)
            } else {
                (cursor_scope, selection_scope)
            };

            // Special-case: cursor at end of the rope.
            if range.head == range.anchor && range.head == text.len_chars() {
                spans.push((cursor_scope, range.head..range.head + 1));
                continue;
            }

            let range = range.min_width_1(text);
            if range.head > range.anchor {
                // Standard case.
                let cursor_start = prev_grapheme_boundary(text, range.head);
                spans.push((selection_scope, range.anchor..cursor_start));
                spans.push((cursor_scope, cursor_start..range.head));
            } else {
                // Reverse case.
                let cursor_end = next_grapheme_boundary(text, range.head);
                spans.push((cursor_scope, range.head..cursor_end));
                spans.push((selection_scope, cursor_end..range.anchor));
            }
        }

        spans
    }
    fn build_syntax_highlights(&'a self) -> impl Iterator<Item = HighlightEvent> + 'a {
        let doc = self.editor.document(self.view.doc).unwrap();
        let theme = &self.editor.theme;
        let offset = self.view.offset;
        let loader = &self.editor.syn_loader;
        let text = doc.text().slice(..);
        let area = self.view.area;
        let last_line = std::cmp::min(
            // Saturating subs to make it inclusive zero indexing.
            (offset.row + area.height as usize).saturating_sub(1),
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
                        loader
                                .language_configuration_for_injection_string(language)
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

impl<'a> Widget for ViewWidget<'a> {
    fn ui(self, ui: &mut Ui) -> egui::Response {
        let doc = self.editor.document(self.view.doc).unwrap();
        ui.with_layout(Layout::top_down(egui::Align::Min), |ui| {
            let width = self.view.area.width as f32
                * ui.fonts().glyph_width(egui::TextStyle::Monospace, 'm');
            ui.set_width(width);
            ui.set_height(
                self.view.area.height as f32 * ui.fonts().row_height(egui::TextStyle::Monospace),
            );
            ui.add(DocumentWidget {
                doc,
                offset: self.view.offset,
                area: self.view.inner_area(),
                theme: &self.editor.theme,
                highlights: self.build_highlights(),
            });
            let base_style = if self.focused {
                self.editor.theme.get("ui.statusline")
            } else {
                self.editor.theme.get("ui.statusline.inactive")
            };
            Frame::default()
                .fill(base_style.bg.map(convert_color).unwrap_or(Color32::BLUE))
                .show(ui, |ui| {
                    ui.set_width(width);
                    ui.with_layout(Layout::bottom_up(egui::Align::Min), |ui| {
                        ui.colored_label(
                            base_style.fg.map(convert_color).unwrap_or(Color32::WHITE),
                            match doc.mode() {
                                helix_view::document::Mode::Normal => "NOR",
                                helix_view::document::Mode::Select => "SEL",
                                helix_view::document::Mode::Insert => "INS",
                            },
                        );
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
    theme: &'a Theme,
    highlights: Box<dyn Iterator<Item = HighlightEvent> + 'a>,
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

impl<'a> DocumentWidget<'a> {}

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
            highlights,
            ..
        } = self;
        let line_height = ui.fonts().row_height(egui::TextStyle::Monospace);
        let char_width = ui.fonts().glyph_width(egui::TextStyle::Monospace, 'm');
        let available_rect = ui.available_rect_before_wrap();
        let top_left = available_rect.left_top();
        let mut paint_cursor = top_left;
        let text_style = theme.get("ui.text");
        let mut spans: Vec<Highlight> = Vec::new();
        let text = doc.text().slice(..);

        let mut visual_x = 0u16;
        let mut line = 1u16;
        // Render gutter
        paint_cursor += Vec2::RIGHT * char_width * (5 - dumb_log(line + area.y)) as f32;

        ui.painter().text(
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
                                let res = ui.painter().text(
                                    paint_cursor,
                                    egui::Align2::LEFT_TOP,
                                    trimmed,
                                    egui::TextStyle::Monospace,
                                    style.fg.map(convert_color).unwrap_or(Color32::WHITE),
                                );
                                if style.add_modifier.contains(Modifier::REVERSED) {
                                    if let Some(fg) = style.fg.map(convert_color) {
                                        ui.painter().rect_filled(res, 0., fg);
                                    }
                                }
                                if let Some(bg) = style.bg.map(convert_color) {
                                    ui.painter().rect_filled(res, 0., dbg!(bg));
                                }
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

                            ui.painter().text(
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
