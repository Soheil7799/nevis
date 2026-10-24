//! The command palette (Ctrl+Shift+P): every command, with its keys, to run one by its name.
//! Typing narrows the list; Enter or a click runs it, Esc or a click outside closes it.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::{glib, prelude::*};

use crate::help::Command;
use crate::{popup, switcher};

/// Called with the action of the command picked.
type Run = Box<dyn Fn(&str)>;

pub struct Palette {
    pub widget: gtk::Box,
    entry: gtk::Entry,
    list: gtk::ListBox,
    commands: RefCell<Vec<Command>>,
    /// Which commands the list shows, in order.
    shown: RefCell<Vec<usize>>,
    run: Run,
}

impl Palette {
    pub fn new(run: impl Fn(&str) + 'static) -> Rc<Self> {
        let entry = gtk::Entry::builder().placeholder_text("Run a command").build();
        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).build();
        list.add_css_class("nevis-switcher-list");
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(460)
            .child(&list)
            .build();
        let (widget, card) = popup::card(520);
        card.append(&entry);
        card.append(&scroller);
        popup::browse_with_arrows(&widget, &list, &entry);
        let palette = Rc::new(Self { widget, entry, list, commands: RefCell::new(Vec::new()), shown: RefCell::new(Vec::new()), run: Box::new(run) });
        let weak = Rc::downgrade(&palette);
        palette.entry.connect_changed(move |_| {
            if let Some(palette) = weak.upgrade() {
                palette.fill();
            }
        });
        let weak = Rc::downgrade(&palette);
        palette.entry.connect_activate(move |_| {
            if let Some(palette) = weak.upgrade() {
                palette.go();
            }
        });
        let weak = Rc::downgrade(&palette);
        palette.list.connect_row_activated(move |_, _| {
            if let Some(palette) = weak.upgrade() {
                palette.go();
            }
        });
        palette
    }

    pub fn is_open(&self) -> bool {
        self.widget.is_visible()
    }

    /// Opens on `commands`, those that can run now.
    pub fn show(&self, commands: Vec<Command>, placeholder: &str) {
        *self.commands.borrow_mut() = commands;
        self.entry.set_placeholder_text(Some(placeholder));
        self.entry.set_text("");
        self.fill();
        self.widget.set_visible(true);
        self.entry.grab_focus();
    }

    pub fn hide(&self) {
        self.widget.set_visible(false);
    }

    /// Lists the commands matching what is typed, each with its keys; selects the first.
    fn fill(&self) {
        let commands = self.commands.borrow();
        let names: Vec<String> = commands.iter().map(|command| command.what.to_owned()).collect();
        let shown = switcher::rank(&self.entry.text(), &names);
        while let Some(row) = self.list.first_child() {
            self.list.remove(&row);
        }
        for &index in &shown {
            let command = &commands[index];
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            let what = gtk::Label::builder().label(&command.what).xalign(0.0).hexpand(true).ellipsize(gtk::pango::EllipsizeMode::End).build();
            let keys = gtk::Label::builder().label(&command.keys).xalign(1.0).css_classes(["nevis-help-keys"]).build();
            line.append(&what);
            line.append(&keys);
            self.list.append(&line);
        }
        *self.shown.borrow_mut() = shown;
        if let Some(row) = self.list.row_at_index(0) {
            self.list.select_row(Some(&row));
        }
        // The typing stays in the field.
        let entry = self.entry.clone();
        glib::idle_add_local_once(move || {
            entry.grab_focus_without_selecting();
        });
    }

    /// Runs the selected command.
    fn go(&self) {
        let Some(row) = self.list.selected_row() else { return };
        let action = self.shown.borrow().get(row.index() as usize).and_then(|&index| self.commands.borrow().get(index).map(|command| command.action.clone()));
        if let Some(action) = action {
            self.hide();
            (self.run)(&action);
        }
    }
}
