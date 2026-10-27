//! A small question with a text field, drawn over the window ("Rename to…", "New note").
//! Enter accepts, Esc or a click outside cancels; if the text can't be used, the reason shows
//! under the field and the question stays open.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;

use crate::popup;

/// Turns what was typed into the value to use, or says why it can't be used.
type Check = Box<dyn Fn(&str) -> Result<String, String>>;
/// Gets the value to use once it was accepted.
type Done = Box<dyn Fn(String)>;

pub struct Prompt {
    pub widget: gtk::Box,
    title: gtk::Label,
    entry: gtk::Entry,
    error: gtk::Label,
    check: RefCell<Option<Check>>,
    done: RefCell<Option<Done>>,
}

impl Prompt {
    pub fn new() -> Rc<Self> {
        let title = gtk::Label::builder().xalign(0.0).css_classes(["nevis-prompt-title"]).build();
        let entry = gtk::Entry::new();
        let error = gtk::Label::builder().xalign(0.0).wrap(true).css_classes(["nevis-prompt-error"]).build();
        let (widget, card) = popup::card(420);
        card.append(&title);
        card.append(&entry);
        card.append(&error);

        let prompt = Rc::new(Self {
            widget,
            title,
            entry,
            error,
            check: RefCell::new(None),
            done: RefCell::new(None),
        });

        let weak = Rc::downgrade(&prompt);
        prompt.entry.connect_activate(move |_| {
            if let Some(prompt) = weak.upgrade() {
                prompt.accept();
            }
        });
        let weak = Rc::downgrade(&prompt);
        prompt.entry.connect_changed(move |_| {
            if let Some(prompt) = weak.upgrade() {
                prompt.error.set_visible(false);
            }
        });
        prompt
    }

    /// Asks `title`, with `initial` typed in and its name part (before the last ".") selected.
    pub fn ask(
        &self,
        title: &str,
        initial: &str,
        check: impl Fn(&str) -> Result<String, String> + 'static,
        done: impl Fn(String) + 'static,
    ) {
        self.title.set_text(title);
        self.entry.set_text(initial);
        self.error.set_visible(false);
        *self.check.borrow_mut() = Some(Box::new(check));
        *self.done.borrow_mut() = Some(Box::new(done));
        self.widget.set_visible(true);
        self.entry.grab_focus();
        let stem = initial.rfind('.').filter(|&dot| dot > 0).map_or(initial.chars().count(), |dot| initial[..dot].chars().count());
        self.entry.select_region(0, stem as i32);
    }

    pub fn hide(&self) {
        self.widget.set_visible(false);
        self.check.borrow_mut().take();
        self.done.borrow_mut().take();
    }

    fn accept(&self) {
        let verdict = self.check.borrow().as_ref().map(|check| check(&self.entry.text()));
        match verdict {
            Some(Ok(value)) => {
                let done = self.done.borrow_mut().take();
                self.hide();
                if let Some(done) = done {
                    done(value);
                }
            }
            Some(Err(reason)) => {
                self.error.set_text(&reason);
                self.error.set_visible(true);
            }
            None => self.hide(),
        }
    }
}
