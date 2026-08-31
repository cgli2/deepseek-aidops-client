//! 目标编译的候选提出层（L0 结构信号 + L1 语言切分）。
//!
//! # 本模块不判断语义
//!
//! 这里只做两件事：提取跨语言通用的**结构信号**（L0），以及把自然语言切成
//! **候选片段**（L1）。哪个候选是真锚点，由 L2（[`crate::workspace_index`]）
//! 用工作区命中率裁决。
//!
//! 这是与旧实现的根本分野。旧实现靠一张中文名词标记表（里面是字段/按钮/订单/
//! 库存这类业务词）判断"这是不是一个名词"，等于用有限枚举去覆盖无限的用户表达：
//! 每来一个没覆盖的说法就加一个词，加词永无止境且不断污染机制层。
//! **补词表不是解法，换裁判才是。** 因此本模块不再持有任何领域词表——
//! 它们已迁至 `builtin_profile`，仅作为 S2 的冷启动种子，不参与判断。
//!
//! # L0 与 L1 的分工
//!
//! - **L0 结构信号**：标识符、引用字面量、路径、变换契约、结构动作。
//!   判定标准是"这是否为一个可定位锚点"，集合封闭，跨语言跨领域通用。
//! - **L1 语言切分/归一**：无空格语言（CJK）的 n-gram 切分、多词缩写的合并、
//!   虚词剥离。只做切分与归一，不做语义判断。

/// 结构动作：描述"做什么操作"的词。它们在任何领域都存在，且集合封闭，
/// 因此属于 L0。与之相对的是领域词（字段/按钮/订单），后者不在机制层出现。
pub const NAV_ACTION_WORDS: &[&str] = &[
    "优化", "修改", "调整", "修复", "改进", "互换", "对调", "交换", "调换", "排序", "排查", "检查",
    "处理", "新增", "添加", "删除", "移除", "隐藏", "展示", "显示",
];

/// 顺序/位置类结构动作。同样跨领域通用，属于 L0。
const ORDER_ACTION_WORDS: &[&str] = &[
    "互换", "对调", "交换", "调换", "顺序", "位置", "前后", "排序", "放在前面", "排在前面", "放前面",
    "排前面", "移到前面", "提前", "置顶", "swap", "reorder", "exchange", "move front",
    "put first", "re-order",
];

/// 并列分隔：用于识别"被一起枚举的若干对象"。
const PARALLEL_SEPARATORS: [char; 8] = ['，', ',', '、', ';', '；', '\n', ':', '：'];

/// 并列连接词（含英文）。它们连接的是同类对象，因此切出来的片段互为候选。
const PARALLEL_JOINERS: &[&str] = &[" and ", " or ", " & ", "和", "与", "及", "跟"];

/// 重述标记。用户常在原话后补一句"即把 KEY 放在前面"来解释自己的意思——
/// 那是对同一件事的复述，不是第三个被交换的对象，必须在切分前截断。
const RESTATEMENT_MARKERS: &[&str] = &[
    "，即",
    ",即",
    "；即",
    "。即",
    "也就是说",
    "换句话说",
    ", i.e.",
    ", namely",
];

/// 句首虚词/功能词。切分时剥离——它们是语言结构，不是目标的一部分。
/// 这是**语言适配**（L1）而非语义判断：中英文各一组封闭的语法词。
const LEAD_FUNCTION_WORDS: &[&str] = &[
    "the ", "a ", "an ", "of ", "to ", "in ", "on ", "for ", "with ", "and ", "or ", "from ",
    "into ", "order ", "position ", "请", "把", "将", "即", "等", "和", "与", "及", "在", "是", "的",
    "了", "让", "使", "给", "对", "从", "到", "帮", "我", "现在", "需要", "希望",
];

/// 句尾赘语。
const TRAIL_FILLER: &[&str] = &["一下", "等等", "等", "着手", "进行"];

const CJK_START: char = '\u{4e00}';
const CJK_END: char = '\u{9fff}';
/// 候选片段的 CJK n-gram 长度区间。
const MIN_CJK_GRAM: usize = 2;
const MAX_CJK_GRAM: usize = 6;
/// ASCII 多词组合的最大跨度（"API KEY" 需要 2，留出 3 以覆盖 "A B C"）。
const MAX_ASCII_SPAN: usize = 3;
/// 候选总数上限。裁决成本与候选数成正比，需要封顶。
const MAX_CANDIDATES: usize = 240;

pub fn is_cjk(ch: char) -> bool {
    (CJK_START..=CJK_END).contains(&ch)
}

fn is_token_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

/// ASCII token 字符：额外容纳路径与文件名里的分隔符，使 `src/pages/model.tsx`
/// 能作为一个整体候选参与裁决。
fn is_ascii_token_char(ch: char) -> bool {
    is_token_char(ch) || matches!(ch, '.' | '/' | '-')
}

/// 大小写混合的代码符号（camelCase / PascalCase），如 `appCode`、`subAppCode`。
///
/// 这是最强的一类定位信号，同时也是唯一用于"工作区不匹配"严格判定的信号——
/// 中文片段与缩写太泛，0 命中不足以证明仓库里没有目标。
pub fn extract_code_symbols(input: &str) -> Vec<String> {
    input
        .split(|ch: char| !is_token_char(ch))
        .filter(|token| {
            token.len() >= 3
                && token.chars().any(|ch| ch.is_ascii_lowercase())
                && token.chars().any(|ch| ch.is_ascii_uppercase())
        })
        .map(str::to_string)
        .collect()
}

/// 全大写缩写，如 `API`、`KEY`、`URL`。旧实现因要求"必须有小写字母"而整体丢失
/// 这类信号，而中文 UI 项目里字段名恰恰常以缩写出现。
pub fn extract_acronyms(input: &str) -> Vec<String> {
    input
        .split(|ch: char| !is_token_char(ch))
        .filter(|token| {
            token.len() >= 2
                && token.chars().all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
                && token.chars().any(|ch| ch.is_ascii_uppercase())
        })
        .map(str::to_string)
        .collect()
}

/// 把 `>` / `→` 路径拆成"可定位导航词"与"动作描述"两堆。
///
/// 旧实现把 `->` 前的第一段无条件当导航词，于是"界面优化"被拿去做源码
/// contains 扫描——它是用户动作，不可能命中，白白烧掉一次全量扫描。
pub fn extract_navigation(input: &str) -> (Vec<String>, Vec<String>) {
    if !input.contains('>') && !input.contains('→') {
        return (Vec::new(), Vec::new());
    }
    let mut navigation = Vec::new();
    let mut actions = Vec::new();
    for raw in input.split(['>', '→']) {
        let part = raw.trim();
        let first = part
            .split(PARALLEL_SEPARATORS)
            .next()
            .unwrap_or(part)
            .trim();
        if first.is_empty() || first.chars().count() > 32 {
            continue;
        }
        if NAV_ACTION_WORDS.iter().any(|word| first.contains(word)) {
            actions.push(first.to_string());
        } else {
            navigation.push(first.to_string());
        }
    }
    navigation.truncate(6);
    actions.truncate(4);
    (navigation, actions)
}

/// L1：把输入切成候选片段，**不做任何语义判断**。
///
/// 中文没有空格，因此按字符类型边界切出 n-gram；英文按空白切成 token 后再做
/// 最多 3 词的连续组合，使 "Model Name"、"API KEY" 这类多词表达不被拆断。
/// 产出的候选全部交给 L2 用工作区命中率裁决——存在的留下，不存在的淘汰。
pub fn segment_candidates(input: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    push_cjk_ngrams(input, &mut out);
    push_ascii_spans(input, &mut out);
    dedup(&mut out);
    out.truncate(MAX_CANDIDATES);
    out
}

fn push_cjk_ngrams(input: &str, out: &mut Vec<String>) {
    let chars: Vec<char> = input.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        if !is_cjk(chars[index]) {
            index += 1;
            continue;
        }
        let start = index;
        while index < chars.len() && is_cjk(chars[index]) {
            index += 1;
        }
        let run = &chars[start..index];
        let max_len = MAX_CJK_GRAM.min(run.len());
        for size in MIN_CJK_GRAM..=max_len {
            for window in run.windows(size) {
                out.push(window.iter().collect::<String>());
            }
        }
    }
}

fn push_ascii_spans(input: &str, out: &mut Vec<String>) {
    let chars: Vec<char> = input.chars().collect();
    let mut groups: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        if is_ascii_token_char(chars[index]) {
            let start = index;
            while index < chars.len() && is_ascii_token_char(chars[index]) {
                index += 1;
            }
            let token: String = chars[start..index].iter().collect();
            if token.chars().any(|ch| ch.is_ascii_alphanumeric()) {
                current.push(token);
            }
        } else if chars[index] == ' ' && !current.is_empty() {
            // 空格两侧都是 ASCII token 时它只是多词缩写的内部分隔
            // （"API KEY"），必须延续同一组，否则 "API KEY" 会被拆成 "KEY"。
            let mut next = index;
            while next < chars.len() && chars[next] == ' ' {
                next += 1;
            }
            if next < chars.len() && is_ascii_token_char(chars[next]) {
                index = next;
            } else {
                groups.push(std::mem::take(&mut current));
                index = next;
            }
        } else {
            if !current.is_empty() {
                groups.push(std::mem::take(&mut current));
            }
            index += 1;
        }
    }
    if !current.is_empty() {
        groups.push(current);
    }

    for group in groups {
        let max_span = MAX_ASCII_SPAN.min(group.len());
        for size in 1..=max_span {
            for window in group.windows(size) {
                out.push(window.join(" "));
            }
        }
    }
}

fn dedup(items: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    items.retain(|item| seen.insert(item.clone()));
}

/// 剥离句首虚词与句尾赘语。只做语言归一（L1），不做语义判断。
fn normalize_item(text: &str) -> String {
    let mut current = text.trim().to_string();
    loop {
        let before = current.clone();
        for word in LEAD_FUNCTION_WORDS {
            if current.starts_with(word) && current.chars().count() > word.chars().count() {
                current = current[word.len()..].trim_start().to_string();
            }
        }
        for filler in TRAIL_FILLER {
            if current.ends_with(filler) && current.chars().count() > filler.chars().count() {
                current = current[..current.len() - filler.len()].trim_end().to_string();
            }
        }
        if current == before {
            break;
        }
    }
    current
}

/// 去掉动作词，并返回是否确实去掉过。
fn strip_action_words(segment: &str) -> (String, bool) {
    let mut text = segment.to_string();
    let mut removed = false;
    for word in ORDER_ACTION_WORDS.iter().chain(NAV_ACTION_WORDS.iter()) {
        if text.contains(word) {
            removed = true;
            text = text.replace(word, "");
        }
    }
    (text, removed)
}

fn split_parallel(text: &str) -> Vec<String> {
    let mut parts: Vec<String> = vec![text.to_string()];
    for joiner in PARALLEL_JOINERS {
        let mut next = Vec::new();
        for part in &parts {
            let mut rest: &str = part;
            while let Some(pos) = rest.find(joiner) {
                next.push(rest[..pos].to_string());
                rest = &rest[pos + joiner.len()..];
            }
            next.push(rest.to_string());
        }
        parts = next;
    }
    let mut out = Vec::new();
    for part in &parts {
        for piece in part.split(PARALLEL_SEPARATORS) {
            out.push(piece.to_string());
        }
    }
    out
}

/// 从"顺序调整"类请求里提取被并列枚举的对象。
///
/// 判定依据是**并列结构**而非领域词：旧实现以"字段"为锚点回溯标签，那是一个
/// 领域词，换成英文或换成"列/参数/卡片"就失效。这里改为——只要出现顺序类结构
/// 动作，就把该句中被并列枚举的对象切出来，再交给工作区裁决它们的实际形式。
pub fn extract_parallel_items(input: &str) -> Vec<String> {
    let mentions_order = ORDER_ACTION_WORDS.iter().any(|word| input.contains(word));
    if !mentions_order {
        return Vec::new();
    }
    let mut scope = input.to_string();
    for marker in RESTATEMENT_MARKERS {
        if let Some(pos) = scope.find(marker) {
            scope.truncate(pos);
        }
    }
    let mut items: Vec<String> = Vec::new();
    for segment in split_parallel(&scope) {
        // 导航路径说明的是"在哪个界面"，不是被交换的对象。
        if segment.contains('>') || segment.contains('→') {
            continue;
        }
        let (stripped, removed_action) = strip_action_words(&segment);
        let cleaned = normalize_item(&stripped);
        // 剥掉动作词后只剩两三个字，说明这段本身就是动作描述
        // （"界面优化" → "界面"），不是被交换的对象。
        if removed_action && cleaned.chars().count() < 3 {
            continue;
        }
        if cleaned.chars().count() < 2 {
            continue;
        }
        if !items.contains(&cleaned) {
            items.push(cleaned);
        }
    }
    // 去掉被其它项包含的重复项（"KEY" 与 "API KEY字段" 同时出现时保留更完整的后者）。
    let contained: Vec<String> = items
        .iter()
        .filter(|item| items.iter().any(|other| other != *item && other.contains(item.as_str())))
        .cloned()
        .collect();
    items.retain(|item| !contained.contains(item));
    items.truncate(4);
    items
}

/// 字段/元素顺序的调整方式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldOrderIntent {
    /// 整体反转（互换 / 对调 / 交换）。
    Reverse,
    /// 把指定项移到最前（"把 KEY 放在前面"）。
    MoveFront(String),
}

/// 并列对象的顺序变更。这是高确定性任务：一旦编译出来，Agent 只需定位同时包含
/// 这些对象的定义并交换渲染顺序，不需要任何探索性搜索。
///
/// `fields` 保存**用户原话**，代码里的实际形式由 `resolved` 承载——解析发生在
/// 工作区裁决之后，不由词表猜测。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormFieldOrder {
    pub fields: Vec<String>,
    pub intent: FieldOrderIntent,
    resolved: Vec<String>,
}

impl FormFieldOrder {
    /// 目标顺序，以原始 fields 的下标表示。
    pub fn desired_order(&self) -> Vec<usize> {
        match &self.intent {
            FieldOrderIntent::Reverse => (0..self.fields.len()).rev().collect(),
            FieldOrderIntent::MoveFront(label) => {
                let mut rest: Vec<usize> = (0..self.fields.len()).collect();
                if let Some(pos) = self.fields.iter().position(|field| field == label) {
                    rest.remove(pos);
                    rest.insert(0, pos);
                }
                rest
            }
        }
    }

    /// 工作区裁决后确定的实际形式；未裁决时回落到用户原话。
    pub fn effective_fields(&self) -> &[String] {
        if self.resolved.len() == self.fields.len() {
            &self.resolved
        } else {
            &self.fields
        }
    }

    /// 用工作区裁决每项的实际形式。用户说"模型名称字段"，代码里可能是
    /// `modelName` 或"模型名称"——**由工作区决定，不由词表决定**。
    ///
    /// 以闭包形式注入裁决器，使 L1 不依赖 L2：本模块可以独立编译与测试。
    pub fn resolve_with<F>(&mut self, mut resolver: F)
    where
        F: FnMut(&[String]) -> Option<String>,
    {
        self.resolved = self
            .fields
            .iter()
            .map(|field| {
                let mut candidates = segment_candidates(field);
                candidates.insert(0, field.clone());
                resolver(&candidates).unwrap_or_else(|| field.clone())
            })
            .collect();
    }

    /// 渲染给模型的结构化指令。
    pub fn render_for_model(&self) -> String {
        let fields = self.effective_fields();
        let order = self
            .desired_order()
            .iter()
            .filter_map(|index| fields.get(*index))
            .cloned()
            .collect::<Vec<_>>()
            .join(" → ");
        format!(
            "[顺序调整目标] 目标对象：{}；期望顺序：{}。定位同时包含这些对象的定义，按期望顺序调整渲染次序即可，无需探索性搜索。",
            fields.join("、"),
            order
        )
    }
}

/// 仅当出现顺序类结构动作、且切出至少两个并列对象时才产出结构化目标。
pub fn extract_form_field_order(input: &str) -> Option<FormFieldOrder> {
    let fields = extract_parallel_items(input);
    if fields.len() < 2 {
        return None;
    }
    let intent = if let Some(label) = fields.iter().find(|label| {
        input.contains(&format!("{label}放在前面"))
            || input.contains(&format!("{label}排在前面"))
            || input.contains(&format!("{label}放前面"))
            || input.contains(&format!("{label}排前面"))
            || input.contains(&format!("{label}移到前面"))
            || input.contains(&format!("把{label}放"))
            || input.contains(&format!("{label} first"))
            || input.contains(&format!("{label} before"))
    }) {
        FieldOrderIntent::MoveFront(label.clone())
    } else {
        FieldOrderIntent::Reverse
    };
    Some(FormFieldOrder {
        fields,
        intent,
        resolved: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str =
        "界面优化，系统管理->模型管理，把模型名称字段，API KEY字段位置顺序互换一下，即把KEY放在排在前面。";

    #[test]
    fn candidates_are_segmented_without_semantic_judgment() {
        let candidates = segment_candidates(SAMPLE);
        // 关键：候选既包含动作片段也包含对象片段——判断谁有用是 L2 的事，
        // 这里不做取舍。
        assert!(candidates.contains(&"模型名称".to_string()), "{candidates:?}");
        assert!(candidates.contains(&"界面优化".to_string()), "{candidates:?}");
        assert!(candidates.contains(&"API KEY".to_string()), "{candidates:?}");
    }

    #[test]
    fn multiword_acronyms_are_not_split() {
        let candidates = segment_candidates(SAMPLE);
        assert!(
            candidates.contains(&"API KEY".to_string()),
            "多词缩写必须整体保留，否则裁决时只能拿到 KEY，实际 {candidates:?}"
        );
    }

    #[test]
    fn no_domain_vocabulary_tables_remain_in_the_mechanism() {
        // V5 设计第二原则的可执行断言：机制层不再持有任何领域词表。
        // 它们已迁到 builtin_profile，仅作为 S2 的冷启动种子，不参与判断。
        let source = include_str!("target_extract.rs");
        // 只看非测试部分，否则断言里的表名会把自己匹配出来。
        let source = source.split("#[cfg(test)]").next().unwrap_or(source);
        for table in ["CJK_NOUN_MARKERS", "CJK_ACTION_TAILS", "CJK_LEAD_STOPWORDS"] {
            assert!(
                !source.contains(table),
                "领域词表 {table:?} 不应留在机制层参与判断"
            );
        }
        assert!(
            !source.contains("use crate::builtin_profile"),
            "机制层不得反向依赖领域画像"
        );
    }

    #[test]
    fn acronyms_survive_extraction() {
        let acronyms = extract_acronyms(SAMPLE);
        assert!(acronyms.contains(&"API".to_string()), "{acronyms:?}");
        assert!(acronyms.contains(&"KEY".to_string()), "{acronyms:?}");
    }

    #[test]
    fn camel_case_symbols_still_work() {
        let symbols = extract_code_symbols("列表没有把appCode和subAppCode展示出来");
        assert_eq!(symbols, vec!["appCode", "subAppCode"]);
    }

    #[test]
    fn navigation_drops_the_action_segment() {
        let (navigation, actions) = extract_navigation(SAMPLE);
        assert!(navigation.contains(&"模型管理".to_string()), "{navigation:?}");
        assert!(!navigation.iter().any(|n| n.contains("优化")), "{navigation:?}");
        assert!(actions.iter().any(|a| a.contains("优化")), "{actions:?}");
    }

    #[test]
    fn parallel_items_are_found_without_a_domain_anchor() {
        let items = extract_parallel_items(SAMPLE);
        assert_eq!(items, vec!["模型名称字段", "API KEY字段"]);
    }

    #[test]
    fn english_swap_uses_the_same_structural_path() {
        let items = extract_parallel_items("swap the order of Model Name and API Key fields");
        assert_eq!(items, vec!["Model Name", "API Key fields"]);
    }

    #[test]
    fn field_swap_compiles_into_structured_goal() {
        let order = extract_form_field_order(SAMPLE).expect("should detect field swap");
        assert_eq!(order.fields, vec!["模型名称字段", "API KEY字段"]);
        assert_eq!(order.desired_order(), vec![1, 0]);
    }

    #[test]
    fn workspace_resolves_the_actual_form_of_each_item() {
        let mut order = extract_form_field_order(SAMPLE).expect("should detect field swap");
        // 模拟工作区裁决：只认得代码里真实存在的写法。
        order.resolve_with(|candidates| {
            ["模型名称", "API KEY"]
                .iter()
                .find(|known| candidates.iter().any(|c| c == *known))
                .map(|known| known.to_string())
        });
        assert_eq!(order.effective_fields(), ["模型名称", "API KEY"]);
        let rendered = order.render_for_model();
        assert!(rendered.contains("模型名称"), "{rendered}");
        assert!(rendered.contains("API KEY"), "{rendered}");
    }

    #[test]
    fn plain_change_without_order_words_is_not_a_field_order() {
        assert!(extract_form_field_order("把模型名称字段的校验规则改一下").is_none());
    }
}
