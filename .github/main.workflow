workflow "push" {
  resolves = ["Build"]
  on = "push"
}

action "Build" {
  uses = "icepuma/rust-github-actions/build@master"
}
