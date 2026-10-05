//! Local candidates extracted from user-authored text, never confirmed facts.
//! The surrounding memory store owns source evidence and cross-message history.

use ai_ex_domain::{MemoryEntity, MemoryEntityKind};
use regex::{Regex, RegexBuilder};
use std::sync::LazyLock;

const MAX_INPUT_BYTES: usize = 128 * 1024;
const MAX_CLAUSE_BYTES: usize = 4096;
const MAX_ENTITIES: usize = 24;
const MAX_SERIALIZED_BYTES: usize = 8192;

#[derive(Clone, Copy)]
enum Output {
    Name { explicit: bool },
    Preference(&'static str),
    Fact(&'static str),
}
struct Rule {
    regex: Regex,
    output: Output,
    confidence: u16,
}
fn regex(pattern: &str) -> Regex {
    RegexBuilder::new(pattern)
        .case_insensitive(true)
        .build()
        .expect("static entity pattern must be valid")
}
static RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    use Output::{Fact, Name, Preference};
    [
        (r"^(?:my (?:real )?name is(?: actually)?|my name's|i'm called|i am called|(?:please )?call me|you can call me|i go by)\s+(?P<value>[\p{L}][\p{L}' -]{1,79})$", Name { explicit: true }, 950),
        (r"^i(?:'m| am)\s+(?P<value>[\p{L}][\p{L}' -]{1,59})$", Name { explicit: false }, 850),
        (r"^(?:我叫|我名叫|我的名字(?:是|叫))(?P<value>[\p{L}][\p{L}' -]{1,39})$", Name { explicit: true }, 950),
        // Explicit polarity avoids the old second-pass negation inversion.
        (r"^i\s+(?:absolutely |really |truly )?(?:love|adore|enjoy|like|prefer|am fond of|am into|am crazy about|am obsessed with)\s+(?P<value>.+)$", Preference("like"), 900),
        (r"^i'm\s+(?:a fan of|crazy about|passionate about|obsessed with|really into)\s+(?P<value>.+)$", Preference("like"), 900),
        (r"^i\s+(?:absolutely |really |truly )?(?:hate|despise|loathe|detest|dislike|can't stand|cannot stand|don't like|do not like|am not fond of|am not into|avoid|stay away from|won't touch)\s+(?P<value>.+)$", Preference("dislike"), 900),
        (r"^i'm\s+(?:not a fan of|not into|sick of|tired of|fed up with)\s+(?P<value>.+)$", Preference("dislike"), 850),
        (r"^i\s+(?:sometimes|occasionally|sort of|kind of|somewhat)\s+(?:like|enjoy)\s+(?P<value>.+)$", Preference("neutral"), 700),
        (r"^i(?:'m| am)\s+(?:neutral about|indifferent to|ambivalent about)\s+(?P<value>.+)$", Preference("neutral"), 850),
        (r"^i\s+(?:don't mind|have no opinion about|have mixed feelings about)\s+(?P<value>.+)$", Preference("neutral"), 800),
        (r"^my (?:favorite|favourite)\s+.+?\s+is\s+(?P<value>.+)$", Preference("like"), 950),
        (r"^(?P<value>.+?)\s+is my (?:favorite|favourite)$", Preference("like"), 900),
        (r"^(?P<value>.+?)\s+(?:is|are)\s+(?:amazing|awesome|incredible|fantastic|wonderful|perfect|the best|outstanding|excellent|brilliant|marvelous)$", Preference("like"), 750),
        (r"^(?P<value>.+?)\s+(?:is|are)\s+(?:terrible|awful|horrible|disgusting|revolting|repulsive|the worst|gross|nasty|dreadful)$", Preference("dislike"), 750),
        (r"^(?P<value>.+?)\s+(?:is|are)\s+(?:okay|fine|alright|not bad|decent|average|so-so|pretty good|quite nice|rather nice|fairly good)$", Preference("neutral"), 700),
        (r"^(?P<value>.+?)\s+(?:rocks|rules)$", Preference("like"), 750),
        (r"^(?P<value>.+?)\s+sucks$", Preference("dislike"), 750),
        (r"^(?P<value>.+?)\s+makes me\s+(?:happy|excited|joyful|thrilled)$", Preference("like"), 800),
        (r"^(?P<value>.+?)\s+makes me\s+(?:sick|angry|upset|frustrated|annoyed)$", Preference("dislike"), 800),
        (r"^i get\s+(?:happy|excited|thrilled)\s+about\s+(?P<value>.+)$", Preference("like"), 800),
        (r"^i get\s+(?:angry|upset|frustrated|annoyed)\s+(?:about|with)\s+(?P<value>.+)$", Preference("dislike"), 800),
        (r"^我(?:也|很|最|非常|特别|特別|真的|比较|比較|更)?(?:喜欢|喜歡|喜爱|喜愛|爱|愛|偏爱|偏愛)(?P<value>.+)$", Preference("like"), 900),
        (r"^我(?:也|很|非常|真的)?(?:不喜欢|不喜歡|讨厌|討厭|厌恶|厭惡|不爱|不愛|受不了)(?P<value>.+)$", Preference("dislike"), 900),
        (r"^我(?:偶尔|偶爾|有时|有時)(?:喜欢|喜歡)(?P<value>.+)$", Preference("neutral"), 700),
        (r"^我(?:对|對)(?P<value>.+?)(?:没意见|沒意見|无所谓|無所謂|感觉一般|感覺一般)$", Preference("neutral"), 800),
        (r"^我最(?:喜欢|喜歡)的.+?是(?P<value>.+)$", Preference("like"), 950),
        (r"^(?:i(?:'m| am)|my age is|i just turned)\s+(?P<value>\d{1,3})(?:\s+years?\s+old)?$", Fact("age"), 950),
        (r"^(?:i (?:live|am living|reside) (?:in|at)|my (?:city|hometown|home|address) is|i(?:'m| am) (?:from|based in|located in|currently in)|i moved to)\s+(?P<value>.+)$", Fact("location"), 900),
        (r"^(?:i (?:work|am working) as (?:a|an)|my (?:job|work|career|profession) is|i(?:'m| am) employed as (?:a|an)|i(?:'m| am) (?:a|an))\s+(?P<value>.+?)(?:\s+by profession)?$", Fact("occupation"), 850),
        (r"^i do\s+(?P<value>.+?)\s+for (?:work|a living)$", Fact("occupation"), 850),
        (r"^(?:i (?:study|am studying|major in|studied|majored in)|my major is|i graduated (?:with|in))\s+(?P<value>.+?)(?:\s+at\s+.+)?$", Fact("education"), 900),
        (r"^i have a (?:degree|bachelor's|master's|phd) in\s+(?P<value>.+)$", Fact("education"), 900),
        (r"^i(?:'m| am) (?:pursuing|getting) (?:a|my)\s+(?P<value>.+?)\s+degree$", Fact("education"), 850),
        (r"^i went to\s+(?P<value>.+?)\s+(?:university|college|school)$", Fact("education"), 800),
        (r"^i (?:play|do|practice|enjoy)\s+(?P<value>.+?)(?:\s+(?:regularly|often|sometimes))?$", Fact("activity"), 800),
        (r"^my (?:hobby|hobbies|interest|interests|passion) (?:is|are|include)\s+(?P<value>.+)$", Fact("activity"), 900),
        (r"^i(?:'m| am) (?:into|interested in|passionate about)\s+(?P<value>.+)$", Fact("activity"), 800),
        (r"^i spend (?:my )?(?:free )?time\s+(?P<value>.+)$", Fact("activity"), 800),
        (r"^i love (?:to )?(?P<value>.+?)\s+in my free time$", Fact("activity"), 800),
        (r"^i have\s+(?P<value>(?:a|an|one|two|three|four|\d+)\s+(?:brothers?|sisters?|siblings?|sons?|daughters?|kids?|children|child|dogs?|cats?|pets?)(?:\s+named\s+[\p{L} -]+)?)$", Fact("family"), 850),
        (r"^my\s+(?P<value>(?:spouse|partner|husband|wife|mother|father|brother|sister|son|daughter)\s+(?:is|lives in)\s+.+)$", Fact("family"), 850),
        (r"^i live with my\s+(?P<value>.+)$", Fact("family"), 800),
        (r"^i(?:'m| am)\s+(?P<value>married|single|divorced|engaged)(?:\s+to\s+.+)?$", Fact("relationship_status"), 900),
        (r"^(?:i(?:'m| am)|my height is)\s+(?P<value>\d+(?:\.\d+)?\s*(?:feet|ft|meters?|m|centimeters?|cm))(?:\s+(?:tall|high))?$", Fact("height"), 900),
        (r"^i have\s+(?P<value>brown|blue|green|hazel|gray|black)\s+eyes$", Fact("eye_color"), 900),
        (r"^my eyes are\s+(?P<value>brown|blue|green|hazel|gray|black)$", Fact("eye_color"), 900),
        (r"^i have\s+(?P<value>brown|black|blonde|red|gray|white)\s+hair$", Fact("hair_color"), 900),
        (r"^my hair is\s+(?P<value>brown|black|blonde|red|gray|white)$", Fact("hair_color"), 900),
        (r"^i(?:'m| am)\s+(?P<value>left|right)[ -]handed$", Fact("handedness"), 900),
        (r"^i(?:'m| am) (?:a )?(?P<value>vegetarian|vegan|pescatarian)$", Fact("diet"), 900),
        (r"^(?:i (?:speak|am fluent in)|my native language is)\s+(?P<value>.+)$", Fact("languages"), 900),
        (r"^i was born (?:in|on)\s+(?P<value>.+)$", Fact("birthplace"), 900),
        (r"^(?:my (?:email|email address) is|you can (?:email|contact) me at)\s+(?P<value>[a-z0-9._%+-]+@[a-z0-9.-]+\.[a-z]{2,})$", Fact("email"), 950),
        (r"^my phone (?:number )?is\s+(?P<value>\+?[0-9 ()-]+)$", Fact("phone"), 900),
        (r"^我(?:今年|现在|現在)?(?P<value>\d{1,3})岁$", Fact("age"), 950),
        (r"^我的(?:年龄|年齡)是(?P<value>\d{1,3})(?:岁|歲)?$", Fact("age"), 950),
        (r"^我(?:现在|現在|目前)?(?:住在|居住在|定居在|来自|來自)(?P<value>.+)$", Fact("location"), 900),
        (r"^我的(?:工作|职业|職業)是(?P<value>.+)$", Fact("occupation"), 900),
        (r"^我是(?:一名|一位|一个|一個)?(?P<value>[\p{Han}a-z ]{0,20}(?:教师|老師|老师|医生|醫生|护士|護理師|工程师|工程師|程序员|程式設計師|学生|學生|设计师|設計師|律师|律師|作家|经理|經理))$", Fact("occupation"), 850),
        (r"^(?:我的(?:专业|專業)是|我(?:在学习|在學習|学习|學習|主修))(?P<value>.+)$", Fact("education"), 900),
        (r"^(?:我的(?:爱好|愛好|兴趣|興趣)是|我(?:经常|經常)?(?:玩|练习|練習))(?P<value>.+)$", Fact("activity"), 850),
        (r"^我有(?P<value>(?:一|两|兩|三|\d+)(?:个|個|只|位)?(?:哥哥|姐姐|弟弟|妹妹|兄弟|姐妹|孩子|儿子|兒子|女儿|女兒|猫|貓|狗|宠物|寵物))$", Fact("family"), 850),
        (r"^我(?:是|已经|已經)?(?P<value>已婚|单身|單身|离婚|離婚|订婚|訂婚)$", Fact("relationship_status"), 900),
        (r"^我的身高是?(?P<value>\d+(?:\.\d+)?\s*(?:米|厘米|公分))$", Fact("height"), 900),
        (r"^我的眼睛是(?P<value>棕色|蓝色|藍色|绿色|綠色|灰色|黑色)$", Fact("eye_color"), 900),
        (r"^我的头发是(?P<value>棕色|黑色|金色|红色|紅色|灰色|白色)$", Fact("hair_color"), 900),
        (r"^我是(?P<value>左撇子|右撇子)$", Fact("handedness"), 900),
        (r"^我是(?P<value>素食者|纯素食者|純素食者|鱼素主义者|魚素主義者)$", Fact("diet"), 900),
        (r"^(?:我会说|我會說|我的母语是|我的母語是)(?P<value>.+)$", Fact("languages"), 900),
        (r"^我出生(?:在|于|於)(?P<value>.+)$", Fact("birthplace"), 900),
        (r"^我的(?:邮箱|郵箱|电子邮箱|電子郵箱)是(?P<value>[a-z0-9._%+-]+@[a-z0-9.-]+\.[a-z]{2,})$", Fact("email"), 950),
        (r"^我的(?:电话|電話|电话号码|電話號碼|手机号|手機號|手机号码|手機號碼)是(?P<value>\+?[0-9 ()-]+)$", Fact("phone"), 900),
    ].into_iter().map(|(pattern, output, confidence)| Rule {
        regex: regex(pattern), output, confidence,
    }).collect()
});
static CLAUSE_BOUNDARY: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?:[,，]\s*|\s+(?:and|but|however)\s+|(?:而且|但是|并且|並且|但))(?P<subject>i\b|my\b|我)",
    )
});
static NONASSERTIVE: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"\b(?:if|unless|suppose|imagine|wish|perhaps|maybe|might|would|could)\b|(?:假如|如果|假设|假設|也许|也許|可能|希望|但愿|但願|例如|比如)",
    )
});
static LIST_BOUNDARY: LazyLock<Regex> = LazyLock::new(|| regex(r"\s+(?:and|or)\s+|[,、]|以及|和"));
static GREETING: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"^(?:(?:hi|hello|hey|actually|now|well|okay|sorry)\s*[,，]?\s+|(?:你好|其实|其實|现在|現在)[,，]?\s*)",
    )
});

pub(crate) fn extract_entities(user_text: &str) -> Vec<MemoryEntity> {
    let mut end = user_text.len().min(MAX_INPUT_BYTES);
    while !user_text.is_char_boundary(end) {
        end -= 1;
    }
    let input = &user_text[..end];
    let mut entities = Vec::new();
    let mut start = 0;
    for (offset, character) in input.char_indices() {
        let after = offset + character.len_utf8();
        let boundary = matches!(
            character,
            '\n' | '!' | '?' | ';' | '。' | '！' | '？' | '；'
        ) || (character == '.'
            && input[after..]
                .chars()
                .next()
                .is_none_or(char::is_whitespace));
        if boundary {
            extract_sentence(&input[start..after], &mut entities);
            start = after;
        }
    }
    extract_sentence(&input[start..], &mut entities);
    entities
}

fn extract_sentence(sentence: &str, entities: &mut Vec<MemoryEntity>) {
    let sentence = sentence.trim();
    if sentence.is_empty() || sentence.ends_with(['?', '？']) || NONASSERTIVE.is_match(sentence) {
        return;
    }
    let mut start = 0;
    for captures in CLAUSE_BOUNDARY.captures_iter(sentence) {
        let boundary = captures.get(0).expect("whole clause boundary");
        let subject = captures.name("subject").expect("clause subject");
        extract_clause(&sentence[start..boundary.start()], entities);
        start = subject.start();
    }
    extract_clause(&sentence[start..], entities);
}

fn extract_clause(clause: &str, entities: &mut Vec<MemoryEntity>) {
    if clause.len() > MAX_CLAUSE_BYTES {
        return;
    }
    let clause = clause.trim_matches(|character: char| {
        character.is_whitespace() || ".,!?;。！？，；".contains(character)
    });
    if clause.chars().count() < 3 {
        return;
    }
    let normalized = clause.replace(['\u{2018}', '\u{2019}'], "'");
    let clause = GREETING.replace(&normalized, "");
    for rule in RULES.iter() {
        let Some(captures) = rule.regex.captures(&clause) else {
            continue;
        };
        let Some(value) = captures.name("value") else {
            continue;
        };
        match rule.output {
            Output::Name { explicit } => {
                let name = value.as_str().trim();
                if valid_name(name, explicit) {
                    insert_bounded(
                        entities,
                        MemoryEntity {
                            kind: MemoryEntityKind::Name,
                            key: "user.name".to_owned(),
                            name: name.to_owned(),
                            value: name.to_owned(),
                            confidence: rule.confidence,
                        },
                    );
                }
            }
            Output::Preference(polarity) => {
                for item in LIST_BOUNDARY.split(value.as_str()) {
                    let item = item.trim();
                    if valid_object(item, 100) {
                        insert_bounded(
                            entities,
                            MemoryEntity {
                                kind: MemoryEntityKind::Preference,
                                key: format!("preference:{}", item.to_lowercase()),
                                name: item.to_owned(),
                                value: format!("{polarity}: {item}"),
                                confidence: rule.confidence,
                            },
                        );
                    }
                }
            }
            Output::Fact(attribute) => {
                let multiple = matches!(attribute, "activity" | "family" | "languages");
                let values: Vec<_> = if multiple {
                    LIST_BOUNDARY.split(value.as_str()).collect()
                } else {
                    vec![value.as_str()]
                };
                for value in values {
                    let value = value.trim();
                    if valid_fact(attribute, value) {
                        let key = if multiple {
                            format!("fact:{attribute}:{}", value.to_lowercase())
                        } else {
                            format!("fact:{attribute}")
                        };
                        insert_bounded(
                            entities,
                            MemoryEntity {
                                kind: MemoryEntityKind::Fact,
                                key,
                                name: attribute.to_owned(),
                                value: value.to_owned(),
                                confidence: rule.confidence,
                            },
                        );
                    }
                }
            }
        }
    }
}

fn valid_name(value: &str, explicit: bool) -> bool {
    if !(2..=80).contains(&value.chars().count()) {
        return false;
    }
    let lowercase = value.to_lowercase();
    let invalid = [
        "not",
        "no",
        "never",
        "called",
        "known",
        "user",
        "person",
        "someone",
        "anybody",
        "everyone",
        "nobody",
        "somebody",
        "dr",
        "mr",
        "mrs",
        "ms",
        "prof",
        "professor",
        "the",
        "and",
        "but",
        "or",
        "a",
        "an",
        "from",
        "happy",
        "sad",
        "tired",
        "hungry",
        "fine",
        "okay",
        "ready",
        "alone",
        "married",
        "single",
        "divorced",
        "engaged",
        "vegetarian",
        "vegan",
        "pescatarian",
        "teacher",
        "doctor",
        "engineer",
        "student",
        "retired",
    ];
    if lowercase
        .split_whitespace()
        .any(|word| invalid.contains(&word))
        || [
            "不", "没", "沒", "想", "喜欢", "喜歡", "讨厌", "討厭", "住在", "岁", "歲",
        ]
        .iter()
        .any(|marker| value.contains(marker))
    {
        return false;
    }
    if explicit {
        return true;
    }
    // A bare "I'm" introduces moods and professions too. Require a
    // capitalized name or a known common name, not an arbitrary word.
    let common = [
        "alex",
        "sam",
        "jordan",
        "taylor",
        "casey",
        "riley",
        "avery",
        "quinn",
        "john",
        "jane",
        "mike",
        "sarah",
        "david",
        "lisa",
        "chris",
        "anna",
        "james",
        "mary",
        "michael",
        "jennifer",
        "william",
        "elizabeth",
        "daniel",
        "maria",
        "tom",
        "tim",
        "jim",
        "kim",
        "ben",
        "jen",
        "dan",
        "ann",
        "joe",
        "amy",
        "bob",
        "max",
        "leo",
        "ava",
        "eva",
        "ivy",
        "zoe",
        "mia",
        "alice",
    ];
    common.contains(&lowercase.as_str())
        || value
            .split_whitespace()
            .all(|word| word.chars().next().is_some_and(char::is_uppercase))
}

fn valid_object(value: &str, max_chars: usize) -> bool {
    let lowercase = value.to_lowercase();
    let length = value.chars().count();
    let minimum = if value.is_ascii() { 2 } else { 1 };
    (minimum..=max_chars).contains(&length)
        && value.chars().any(char::is_alphanumeric)
        && !value.chars().any(char::is_control)
        && ![
            "it",
            "that",
            "this",
            "them",
            "they",
            "something",
            "anything",
            "nothing",
            "someone",
            "the",
            "and",
            "or",
        ]
        .contains(&lowercase.as_str())
        && ![
            "not ", "no ", "never ", "don't ", "do not ", "不", "没", "沒", "的",
        ]
        .iter()
        .any(|prefix| lowercase.starts_with(prefix))
        && ![" said ", " says ", " thinks ", " told "]
            .iter()
            .any(|marker| lowercase.contains(marker))
        && !value.ends_with(['吗', '嗎'])
}

fn valid_fact(attribute: &str, value: &str) -> bool {
    if value.chars().count() > 200 || value.chars().any(char::is_control) {
        return false;
    }
    match attribute {
        "age" => value
            .parse::<u16>()
            .is_ok_and(|age| (1..=120).contains(&age)),
        "height" => valid_height(value),
        "phone" => {
            let digits = value.chars().filter(char::is_ascii_digit).count();
            (7..=20).contains(&digits)
        }
        "occupation" => {
            let lowercase = value.to_lowercase();
            valid_object(value, 100)
                && ![
                    "person",
                    "human",
                    "someone",
                    "nobody",
                    "everybody",
                    "fan",
                    "vegetarian",
                    "vegan",
                    "pescatarian",
                ]
                .contains(&lowercase.as_str())
                && !["fan of ", "little ", "bit "]
                    .iter()
                    .any(|prefix| lowercase.starts_with(prefix))
        }
        "location" => {
            valid_object(value, 100)
                && !["here", "there", "somewhere", "nowhere", "anywhere"]
                    .contains(&value.to_lowercase().as_str())
        }
        _ => valid_object(value, 200),
    }
}

fn valid_height(value: &str) -> bool {
    let number_end = value
        .find(|character: char| !character.is_ascii_digit() && character != '.')
        .unwrap_or(value.len());
    let Ok(number) = value[..number_end].parse::<f64>() else {
        return false;
    };
    let unit = value[number_end..].trim().to_lowercase();
    let meters = match unit.as_str() {
        "m" | "meter" | "meters" | "米" => number,
        "cm" | "centimeter" | "centimeters" | "厘米" | "公分" => number / 100.0,
        "ft" | "feet" => number * 0.3048,
        _ => return false,
    };
    (0.5..=3.0).contains(&meters)
}

fn insert_bounded(entities: &mut Vec<MemoryEntity>, candidate: MemoryEntity) {
    if candidate.name.chars().count() > 256
        || candidate.key.chars().count() > 256
        || candidate.value.chars().count() > 512
    {
        return;
    }
    if let Some(index) = entities
        .iter()
        .position(|entity| entity.key == candidate.key)
    {
        // Later explicit statements update the candidate; the store preserves
        // evidence for conflicting claims from separate memory records.
        let previous = std::mem::replace(&mut entities[index], candidate);
        if serde_json::to_vec(entities).map_or(true, |encoded| encoded.len() > MAX_SERIALIZED_BYTES)
        {
            entities[index] = previous;
        }
    } else if entities.len() < MAX_ENTITIES {
        entities.push(candidate);
        if serde_json::to_vec(entities).map_or(true, |encoded| encoded.len() > MAX_SERIALIZED_BYTES)
        {
            entities.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_claim(text: &str, key: &str, value: &str) {
        let entities = extract_entities(text);
        let entity = entities
            .iter()
            .find(|entity| entity.key == key)
            .unwrap_or_else(|| panic!("missing {key} for {text:?}: {entities:?}"));
        assert_eq!(entity.value, value, "{text:?}");
        assert!((1..=1000).contains(&entity.confidence));
    }

    #[test]
    fn extracts_legacy_name_examples_and_explicit_chinese_names() {
        for (text, name) in [
            ("My name is John", "John"),
            ("I'm Sarah", "Sarah"),
            ("I am Michael", "Michael"),
            ("Call me Alex", "Alex"),
            ("Hi, I'm Jessica", "Jessica"),
            ("My name is Alice Smith", "Alice Smith"),
            ("我叫小明", "小明"),
            ("我的名字是小红", "小红"),
        ] {
            assert_claim(text, "user.name", name);
            let entities = extract_entities(text);
            let entity = entities
                .iter()
                .find(|entity| entity.key == "user.name")
                .unwrap();
            assert_eq!(entity.name, name);
            assert_eq!(entity.kind, MemoryEntityKind::Name);
        }
    }

    #[test]
    fn excludes_other_people_pets_hypotheses_negations_and_questions() {
        for text in [
            "Meet John.",
            "This is John.",
            "John's favorite food is pizza.",
            "My dog's name is Buddy.",
            "The character's name is Romeo.",
            "My name is not Alex.",
            "My name is user.",
            "Call me X.",
            "If I live in Boston, I like coffee.",
            "I wish my name was John.",
            "Maybe I am Alex.",
            "I do not live in Boston.",
            "I am not 25 years old.",
            "I like coffee?",
            "我不是小明。",
            "我不住在北京。",
            "如果我住在北京，我喜欢咖啡。",
            "我住在北京吗？",
        ] {
            assert!(extract_entities(text).is_empty(), "{text:?}");
        }
    }

    #[test]
    fn prefers_explicit_polarity_without_double_negation_reversal() {
        for (text, item, polarity) in [
            ("I love pizza", "pizza", "like"),
            ("I really like chocolate", "chocolate", "like"),
            ("I'm a fan of jazz music", "jazz music", "like"),
            ("My favorite color is blue", "blue", "like"),
            ("Coffee is amazing", "Coffee", "like"),
            ("I hate broccoli", "broccoli", "dislike"),
            ("I don't like coffee", "coffee", "dislike"),
            ("I dislike coffee", "coffee", "dislike"),
            ("I can't stand loud music", "loud music", "dislike"),
            ("Spicy food is terrible", "Spicy food", "dislike"),
            ("Pizza is okay", "Pizza", "neutral"),
            ("I sometimes enjoy reading", "reading", "neutral"),
            ("I don't mind coffee", "coffee", "neutral"),
            ("我喜欢咖啡", "咖啡", "like"),
            ("我喜欢茶", "茶", "like"),
            ("我不喜欢咖啡", "咖啡", "dislike"),
            ("我对咖啡无所谓", "咖啡", "neutral"),
        ] {
            assert_claim(
                text,
                &format!("preference:{}", item.to_lowercase()),
                &format!("{polarity}: {item}"),
            );
        }
    }

    #[test]
    fn extracts_all_principal_legacy_fact_categories() {
        for (text, key, value) in [
            ("I am 25 years old.", "fact:age", "25"),
            ("I live in Boston.", "fact:location", "Boston"),
            ("I am from California.", "fact:location", "California"),
            (
                "I'm a software engineer.",
                "fact:occupation",
                "software engineer",
            ),
            ("I work as a teacher.", "fact:occupation", "teacher"),
            ("My major is mathematics.", "fact:education", "mathematics"),
            ("I have a degree in physics.", "fact:education", "physics"),
            ("I play guitar.", "fact:activity:guitar", "guitar"),
            (
                "I have two children.",
                "fact:family:two children",
                "two children",
            ),
            ("I'm married.", "fact:relationship_status", "married"),
            ("I'm 5.8 feet tall.", "fact:height", "5.8 feet"),
            ("My height is 1.75 meters.", "fact:height", "1.75 meters"),
            ("I have brown eyes.", "fact:eye_color", "brown"),
            ("My hair is black.", "fact:hair_color", "black"),
            ("I'm left handed.", "fact:handedness", "left"),
            ("I'm a vegetarian.", "fact:diet", "vegetarian"),
            ("I speak English.", "fact:languages:english", "English"),
            ("I was born in Paris.", "fact:birthplace", "Paris"),
            (
                "My email is alex.smith@example.com.",
                "fact:email",
                "alex.smith@example.com",
            ),
            (
                "My phone number is +1 (202) 555-0182.",
                "fact:phone",
                "+1 (202) 555-0182",
            ),
        ] {
            assert_claim(text, key, value);
        }
    }

    #[test]
    fn supports_explicit_chinese_facts_and_decimal_height() {
        for (text, key, value) in [
            ("我今年25岁。", "fact:age", "25"),
            ("我住在北京。", "fact:location", "北京"),
            ("我的职业是程序员。", "fact:occupation", "程序员"),
            ("我是软件工程师。", "fact:occupation", "软件工程师"),
            ("我的专业是数学。", "fact:education", "数学"),
            ("我的爱好是围棋。", "fact:activity:围棋", "围棋"),
            ("我有两个孩子。", "fact:family:两个孩子", "两个孩子"),
            ("我单身。", "fact:relationship_status", "单身"),
            ("我的身高是175厘米。", "fact:height", "175厘米"),
            ("我的身高是1.75米。", "fact:height", "1.75米"),
            ("我的眼睛是棕色。", "fact:eye_color", "棕色"),
            ("我的头发是黑色。", "fact:hair_color", "黑色"),
            ("我是左撇子。", "fact:handedness", "左撇子"),
            ("我是素食者。", "fact:diet", "素食者"),
            ("我会说英语。", "fact:languages:英语", "英语"),
            ("我出生在上海。", "fact:birthplace", "上海"),
            (
                "我的邮箱是alex@example.com。",
                "fact:email",
                "alex@example.com",
            ),
            ("我的电话是13800000000。", "fact:phone", "13800000000"),
        ] {
            assert_claim(text, key, value);
        }
    }

    #[test]
    fn separates_clauses_and_preserves_multiple_hobbies_languages_and_relatives() {
        let text = "Hi, I'm John and I'm 28 years old. \
                    I live in Seattle, but I like coffee and I dislike broccoli. \
                    I speak English, Spanish and French. My hobbies are chess and guitar. \
                    I have a sister. I have a brother.";
        for (key, value) in [
            ("user.name", "John"),
            ("fact:age", "28"),
            ("fact:location", "Seattle"),
            ("preference:coffee", "like: coffee"),
            ("preference:broccoli", "dislike: broccoli"),
            ("fact:languages:english", "English"),
            ("fact:languages:spanish", "Spanish"),
            ("fact:languages:french", "French"),
            ("fact:activity:chess", "chess"),
            ("fact:activity:guitar", "guitar"),
            ("fact:family:a sister", "a sister"),
            ("fact:family:a brother", "a brother"),
        ] {
            assert_claim(text, key, value);
        }
        assert_claim(
            "我叫小明，我喜欢茶，而且我住在北京。",
            "preference:茶",
            "like: 茶",
        );
    }

    #[test]
    fn stable_keys_apply_later_corrections_within_the_same_user_text() {
        let entities = extract_entities(
            "My name is Alex. Call me Sam. I like coffee. Actually I dislike coffee. \
             I live in Boston. I live in Seattle.",
        );
        assert_eq!(
            entities
                .iter()
                .filter(|entity| entity.key == "user.name")
                .count(),
            1
        );
        assert_eq!(
            entities
                .iter()
                .filter(|entity| entity.key == "preference:coffee")
                .count(),
            1
        );
        assert_eq!(
            entities
                .iter()
                .find(|entity| entity.key == "user.name")
                .unwrap()
                .value,
            "Sam"
        );
        assert_eq!(
            entities
                .iter()
                .find(|entity| entity.key == "preference:coffee")
                .unwrap()
                .value,
            "dislike: coffee"
        );
        assert_eq!(
            entities
                .iter()
                .find(|entity| entity.key == "fact:location")
                .unwrap()
                .value,
            "Seattle"
        );
    }

    #[test]
    fn does_not_mistake_moods_traits_or_historical_claims_for_current_identity() {
        for text in [
            "I'm tired.",
            "I'm single.",
            "I'm a vegetarian.",
            "I am a teacher.",
            "I used to work as a doctor.",
            "I used to live in Boston.",
            "When I was 20 years old.",
            "My age is 0.",
            "I am 200 years old.",
        ] {
            let entities = extract_entities(text);
            assert!(
                entities
                    .iter()
                    .all(|entity| entity.kind != MemoryEntityKind::Name),
                "{text:?}"
            );
            assert!(
                entities.iter().all(|entity| entity.key != "fact:age"),
                "{text:?}"
            );
        }
        assert!(
            !extract_entities("My height is 99 meters.")
                .iter()
                .any(|entity| entity.key == "fact:height")
        );
        assert!(
            !extract_entities("My phone is 123.")
                .iter()
                .any(|entity| entity.key == "fact:phone")
        );
    }

    #[test]
    fn keeps_utf8_boundaries_and_enforces_candidate_and_serialized_budgets() {
        let many = (0..80)
            .map(|index| format!("I like item{index}. "))
            .collect::<String>();
        let entities = extract_entities(&many);
        assert_eq!(entities.len(), MAX_ENTITIES);
        let long = (0..30)
            .map(|index| format!("我喜欢{}{}。", "茶".repeat(90), index))
            .collect::<String>();
        let entities = extract_entities(&long);
        assert!(!entities.is_empty());
        assert!(entities.len() <= MAX_ENTITIES);
        assert!(serde_json::to_vec(&entities).unwrap().len() <= MAX_SERIALIZED_BYTES);
        assert!(
            entities
                .iter()
                .all(|entity| entity.name.chars().count() <= 256
                    && entity.key.chars().count() <= 256
                    && entity.value.chars().count() <= 512
                    && entity.confidence <= 1000)
        );
        let truncated = format!("{}我喜欢茶。", " ".repeat(MAX_INPUT_BYTES - 1));
        assert!(extract_entities(&truncated).is_empty());
        let oversized_clause = format!("{}\nMy name is Alex.", "茶".repeat(2000));
        assert_claim(&oversized_clause, "user.name", "Alex");
    }

    #[test]
    fn produces_deterministic_candidates_and_handles_empty_input() {
        for empty in ["", "  ", "\n", "...", "☕"] {
            assert!(extract_entities(empty).is_empty());
        }
        let text = "My name is Alex. I like coffee. 我会说中文和英语。";
        assert_eq!(
            serde_json::to_vec(&extract_entities(text)).unwrap(),
            serde_json::to_vec(&extract_entities(text)).unwrap()
        );
    }
}
