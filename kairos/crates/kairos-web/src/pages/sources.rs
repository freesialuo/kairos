use crate::app::Notice;
use leptos::prelude::*;

#[component]
pub fn Sources(notice: Notice) -> impl IntoView {
    let _ = notice;
    view! { <h1>"Sources"</h1> }
}
