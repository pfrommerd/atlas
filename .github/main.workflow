workflow "Build on Push" {
  on = "push"
  resolves = ["Build"]
}

action "Build" {
  uses = "icepuma/rust-github-actions/build@master"
}
