use dioxus::prelude::*;

fn main() {
    launch(App);
}

#[component]
fn App() -> Element {
    rsx! {
        main {
            h1 { "Dragon Switch Rust Next" }
            p { "Dioxus Web workspace is ready for migration." }
        }
    }
}
