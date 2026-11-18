use crate::app::Notice;
use leptos::prelude::*;

#[component]
pub fn Settings(notice: Notice) -> impl IntoView {
    let _ = notice;
    view! { <h1>"Settings"</h1> }
}
