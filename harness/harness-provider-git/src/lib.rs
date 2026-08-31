/// 非 Git 仓库检测：把 git 的底层 stderr 转成一句可行动的中文说明，
/// 避免 "fatal: not a git repository..." 原样透传给 UI。
/// 注意：仍返回 Err（非仓库是错误而非空状态，见测试 non_repository_is_an_error_not_an_empty_change_list）。
fn friendly_git_error(stderr: &str) -> Option<Error> {
    if stderr.contains("not a git repository") {
        return Some(Error::Git(
            "当前工作区不是 Git 仓库（未检测到 .git），Git 功能不可用；如需启用请在工作区执行 git init".into(),
        ));
    }
    None
}

fn git_command(repo: &Path, args: &[&str]) -> Command {