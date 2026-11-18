use crate::app::Notice;
use leptos::prelude::*;

#[component]
pub fn Events(notice: Notice) -> impl IntoView {
    let _ = notice;
    view! { <h1>"Events"</h1> }
}
