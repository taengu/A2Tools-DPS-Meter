//! Native dialogs through GTK, the toolkit Tauri itself runs on here.
//!
//! GTK may only be touched from the thread running its main loop, and callers
//! ask from worker threads, so each dialog is handed to that loop and the
//! caller waits for the answer.

use gtk::prelude::*;

pub(super) fn on_gtk_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    // Runs at once when called from the GTK thread itself, so no deadlock.
    gtk::glib::MainContext::default().invoke(move || {
        let _ = tx.send(f());
    });
    rx.recv().ok()
}

fn run(title: &str, message: &str, kind: gtk::MessageType, buttons: gtk::ButtonsType, default_no: bool) -> gtk::ResponseType {
    let (title, message) = (title.to_string(), message.to_string());
    on_gtk_thread(move || {
        let dialog =
            gtk::MessageDialog::new(None::<&gtk::Window>, gtk::DialogFlags::MODAL, kind, buttons, &message);
        dialog.set_title(&title);
        // The overlay stays on top; the question must not open behind it.
        dialog.set_keep_above(true);
        if default_no { dialog.set_default_response(gtk::ResponseType::No); }
        let response = dialog.run();
        dialog.close();
        response
    })
    .unwrap_or(gtk::ResponseType::None)
}

pub fn ask_yes_no(title: &str, message: &str) -> bool {
    run(title, message, gtk::MessageType::Question, gtk::ButtonsType::YesNo, false) == gtk::ResponseType::Yes
}

pub fn ask_yes_no_default_no(title: &str, message: &str) -> bool {
    run(title, message, gtk::MessageType::Question, gtk::ButtonsType::YesNo, true) == gtk::ResponseType::Yes
}

pub fn show_error(title: &str, message: &str) {
    tracing::error!("{title}: {message}");
    run(title, message, gtk::MessageType::Error, gtk::ButtonsType::Ok, false);
}
