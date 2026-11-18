use crate::app::Notice;
use leptos::prelude::*;

#[component]
pub fn Calendar(notice: Notice) -> impl IntoView {
    let _ = notice;
    view! { <h1>"Calendar"</h1> }
}
