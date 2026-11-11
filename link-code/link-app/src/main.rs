fn main() {
    let paths = std::env::args_os().skip(1).map(Into::into).collect();
    link_ui::app::run_with_paths(paths);
}
