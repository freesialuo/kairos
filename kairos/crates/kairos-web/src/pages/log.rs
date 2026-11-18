use crate::app::Notice;
use leptos::prelude::*;

#[component]
pub fn Log(notice: Notice) -> impl IntoView {
    let _ = notice;
    view! { <h1>"Log"</h1> }
}
