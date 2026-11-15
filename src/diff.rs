//! 行级 diff：用于写入配置前的变更预览。

/// 单行差异的类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineKind {
    /// 两侧都存在的上下文行。
    Context,
    /// 新增的行。
    Added,
    /// 删除的行。
    Removed,
}

/// 一行差异内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiffLine {
    /// 差异类型。
    pub kind: LineKind,
    /// 行文本（不含换行符）。
    pub text: String,
}

/// 计算两段文本的行级差异（基于最长公共子序列）。
pub(crate) fn diff_lines(old: &str, new: &str) -> Vec<DiffLine> {
    let old_lines = split_lines(old);
    let new_lines = split_lines(new);

    // lcs[i][j] 表示 old[i..] 与 new[j..] 的最长公共子序列长度。
    let mut lcs = vec![vec![0usize; new_lines.len() + 1]; old_lines.len() + 1];
    for i in (0..old_lines.len()).rev() {
        for j in (0..new_lines.len()).rev() {
            lcs[i][j] = if old_lines[i] == new_lines[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }

    let mut lines = Vec::with_capacity(old_lines.len().max(new_lines.len()));
    let (mut i, mut j) = (0usize, 0usize);
    while i < old_lines.len() && j < new_lines.len() {
        if old_lines[i] == new_lines[j] {
            lines.push(DiffLine {
                kind: LineKind::Context,
                text: old_lines[i].to_string(),
            });
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            lines.push(DiffLine {
                kind: LineKind::Removed,
                text: old_lines[i].to_string(),
            });
            i += 1;
        } else {
            lines.push(DiffLine {
                kind: LineKind::Added,
                text: new_lines[j].to_string(),
            });
            j += 1;
        }
    }
    while i < old_lines.len() {
        lines.push(DiffLine {
            kind: LineKind::Removed,
            text: old_lines[i].to_string(),
        });
        i += 1;
    }
    while j < new_lines.len() {
        lines.push(DiffLine {
            kind: LineKind::Added,
            text: new_lines[j].to_string(),
        });
        j += 1;
    }
    lines
}

/// 按行切分，并兼容 CRLF 换行。
fn split_lines(text: &str) -> Vec<&str> {
    text.lines()
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_text_has_only_context_lines() {
        let lines = diff_lines("[net]\nkey = 1\n", "[net]\nkey = 1\n");
        assert!(lines.iter().all(|line| line.kind == LineKind::Context));
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn changed_line_reports_removed_then_added() {
        let lines = diff_lines("replace-with = \"old\"\nkeep = 1\n", "replace-with = \"new\"\nkeep = 1\n");
        assert_eq!(lines[0].kind, LineKind::Removed);
        assert_eq!(lines[0].text, "replace-with = \"old\"");
        assert_eq!(lines[1].kind, LineKind::Added);
        assert_eq!(lines[1].text, "replace-with = \"new\"");
        assert_eq!(lines[2].kind, LineKind::Context);
    }

    #[test]
    fn empty_old_text_is_all_added() {
        let lines = diff_lines("", "[net]\ngit-fetch-with-cli = true\n");
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|line| line.kind == LineKind::Added));
    }

    #[test]
    fn crlf_and_lf_compare_equal() {
        let lines = diff_lines("a\r\nb\r\n", "a\nb\n");
        assert!(lines.iter().all(|line| line.kind == LineKind::Context));
    }
}
