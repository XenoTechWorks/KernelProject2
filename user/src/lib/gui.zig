//! The graphical toolkit (a working name): windows over the console, drawn with smooth text and
//! shapes, built from widgets. See gui/widgets.zig to make an app, gui/canvas.zig to draw yourself.

pub const Font = @import("gui/font.zig").Font;
pub const Canvas = @import("gui/canvas.zig").Canvas;
pub const Rect = @import("gui/canvas.zig").Rect;
pub const measure = @import("gui/canvas.zig").measure;
pub const Window = @import("gui/window.zig").Window;
pub const Event = @import("gui/window.zig").Event;
pub const Key = @import("gui/window.zig").Key;

const widgets = @import("gui/widgets.zig");
pub const App = widgets.App;
pub const Widget = widgets.Widget;
pub const Callback = widgets.Callback;
pub const theme = widgets.theme;
pub const setText = widgets.setText;
pub const getText = widgets.getText;
pub const setItems = widgets.setItems;
pub const selected = widgets.selected;
pub const setProgress = widgets.setProgress;
pub const TextView = widgets.TextView;
pub const textbuf = @import("textbuf.zig");
pub const textbuf_tab = @import("gui/textview.zig").tab;
