//! 按字符脚本推断词典的语言方向 —— 移植自 `app/services/language_detect.py`。
//!
//! 拉丁字母→en、汉字→中文、假名→ja。区分不了同用拉丁字母的语言（法/德/西语都会
//! 判成 en），识别结果只是默认值，可在词典列表里修改。

use dict_parser::strip_markup;
use std::collections::HashSet;
use std::sync::LazyLock;

/// 有效字符少于这个数时不做判断
const MIN_CHARS: usize = 20;
/// 假名少于这个数不判为日文
const MIN_KANA: usize = 10;
/// 繁体独有字形占 CJK 字符的比例达到此值判为繁体
const TRADITIONAL_RATIO: f64 = 0.01;

// CJK 统一表意文字（含扩展 A 与兼容表意文字）
fn is_cjk(c: char) -> bool {
    matches!(c, '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}' | '\u{f900}'..='\u{faff}')
}

// 平假名 + 片假名
fn is_kana(c: char) -> bool {
    matches!(c, '\u{3040}'..='\u{309f}' | '\u{30a0}'..='\u{30ff}')
}

fn is_latin(c: char) -> bool {
    c.is_ascii_alphabetic()
}

/// 只收繁体独有的字形；制/里/并/于 这类两种字体通用的字必须排除
static TRADITIONAL_ONLY: LazyLock<HashSet<char>> = LazyLock::new(|| {
    "後國學語詞漢義發說這個們為會來時過對開關門問間見現電車長萬與書頭買賣錢銀鐵鳥馬魚龍\
     風雲聲聽讀寫記認識話請謝誰愛歡樂覺習樣點熱讓應該經濟織級紅綠紙線練結給統絲麗嚴豐臨\
     舉麼烏喬鄉亂爭虧亞產畝親億僅從倉儀價眾優偉傳傷倫偽體俠債傾償儲兒兌蘭興養獸內軍農衝\
     決況凍淨減鳳憑凱擊劉則剛創刪劑劍劇勸辦務動勵勞勢勳區醫華協單衛廠廳歷厲壓厭縣參雙變\
     號嘆嚇呂嗎噸啟員嗚詠響啞喚噴團園圍圖圓聖場壞塊堅壩墳墜壘殼處備復夠夾奪奮獎妝婦媽娛\
     嬰孫寧寶實審宮寬賓尋導盡層嘗屬歲豈崗島嶺峽幣帥師帳幫帶廣莊慶庫廢異棄張彌彎彈強歸當\
     錄徹徑憶憂懷態憐總戀懇惡惱懸驚懼慘懲慣願戲戰戶撲執擴掃揚擾拋護報擔擬擇掛損換據撿擲\
     撐擺攝攤敵數斷無舊顯曉暫術機殺雜權條楊極構槍檔橋夢檢樓歐殲殘毆毀畢氣匯湯溝沒淪滬淚\
     潑澤潔灑淺漿濁測瀏渾濃塗濤潤漲漸漁溫灣濕滿滾滯濾濱灘潛滅燈靈災燦爐煉爛煩燒煥爺牽犧\
     猶狽獅獨獄狹環責敗貨質販貪貧貴貸貿費賀賊資賦賭賞賠賴賺賽讚贈贏趙趨躍蹤軌軒轉輪軟轟\
     軸輕載較輔輛輩輝輸辭辯邊遼達遷運還進遠違連遲選遞邏遺鄧郵鄰鄭釋針釘釣鐘鋼鑰欽鉤鑽鈴\
     鉛銅鋁鏟鋪鏈銷鎖鍋鋒銳錯錫鑼錘錦鍵鎮鏡閉閒悶閘鬧聞閥閣閱隊陽陰陣階際陸陳險隨隱難雛\
     雞離霧頁頂項順須頑顧頓頒預領頻顆題顏額飛飯飲館驅駁驗騎騙魯鮮鳴鴉鴨鴿鵝鷹麥黃齊齒齡龜"
        .chars()
        .collect()
});

/// 判断一段文本对应的语言代码；样本不足或无法判断时返回 None。
fn classify(text: &str) -> Option<&'static str> {
    if text.is_empty() {
        return None;
    }
    let cjk = text.chars().filter(|c| is_cjk(*c)).count();
    let latin = text.chars().filter(|c| is_latin(*c)).count();
    // 假名也算有效字符，否则纯假名词头会被当成样本不足
    let kana = text.chars().filter(|c| is_kana(*c)).count();
    if cjk + latin + kana < MIN_CHARS {
        return None;
    }

    // 假名是日文的排他判据，优先于中英判断；要求假名数不低于汉字的 1/3，
    // 避免中文词典里零星假名误判
    if kana >= MIN_KANA && kana * 3 >= cjk {
        return Some("ja");
    }

    if cjk >= latin {
        let traditional = text
            .chars()
            .filter(|c| is_cjk(*c) && TRADITIONAL_ONLY.contains(c))
            .count();
        let threshold = std::cmp::max(1, (cjk as f64 * TRADITIONAL_RATIO) as usize);
        return if traditional >= threshold {
            Some("zh-Hant")
        } else {
            Some("zh-Hans")
        };
    }
    Some("en")
}

pub const FALLBACK_LANG_FROM: &str = "en";
pub const FALLBACK_LANG_TO: &str = "zh-Hans";

/// 返回 (lang_from, lang_to)；任一侧判定不了时该侧为 None。
///
/// lang_from 看词头用什么文字写，lang_to 看释义用什么文字写，于是英汉、汉英、
/// 汉语单语、英英四种常见情形都能区分开。
pub fn detect_language(headwords: &[String], definitions: &[String]) -> (Option<&'static str>, Option<&'static str>) {
    let headword_text = headwords.join("\n");
    // 释义那一侧先剥掉标签与字符实体：MDict 的释义整段是 HTML，标签名/`&nbsp;` 都是
    // 拉丁字符，不剥会把中文释义的拉丁占比显著拉高
    let definition_text = strip_markup(Some(&definitions.join("\n")));
    (
        classify(&headword_text),
        classify(&definition_text),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detect(headwords: &[&str], definitions: &[&str]) -> (Option<&'static str>, Option<&'static str>) {
        let h: Vec<String> = headwords.iter().map(|s| s.to_string()).collect();
        let d: Vec<String> = definitions.iter().map(|s| s.to_string()).collect();
        detect_language(&h, &d)
    }

    #[test]
    fn en_to_zh_hans() {
        let (from, to) = detect(
            &["apple", "banana", "orange", "computer", "dictionary", "language", "university", "development"],
            &["苹果，一种水果", "香蕉", "橙子", "计算机，电脑", "词典，字典", "语言"],
        );
        assert_eq!(from, Some("en"));
        assert_eq!(to, Some("zh-Hans"));
    }

    #[test]
    fn zh_hans_to_en() {
        // 词头合计要 ≥20 个有效字符（MIN_CHARS）
        let (from, to) = detect(
            &["苹果", "香蕉", "橙子", "电脑", "词典", "语言学习", "经济学", "计算机科学", "数据库"],
            &["apple, a fruit", "banana", "orange", "computer", "dictionary", "language"],
        );
        assert_eq!(from, Some("zh-Hans"));
        assert_eq!(to, Some("en"));
    }

    #[test]
    fn zh_hant_detected_by_exclusive_glyphs() {
        let words: Vec<&str> = ["後", "國", "學", "語", "詞", "漢", "義", "發", "說", "這"]
            .iter()
            .cycle()
            .take(30)
            .copied()
            .collect();
        let (from, _) = detect(&words, &[]);
        assert_eq!(from, Some("zh-Hant"));
    }

    #[test]
    fn ja_detected_by_kana() {
        let words: Vec<&str> = ["ひらがな", "カタカナ", "にほんご", "たべる", "のむ", "いく"]
            .iter()
            .cycle()
            .take(12)
            .copied()
            .collect();
        let (from, _) = detect(&words, &[]);
        assert_eq!(from, Some("ja"));
    }

    #[test]
    fn insufficient_sample_returns_none() {
        let (from, to) = detect(&["ab", "cd"], &["xy"]);
        assert_eq!(from, None);
        assert_eq!(to, None);
    }

    #[test]
    fn html_markup_does_not_skew_definition_side() {
        // 实体和标签是拉丁字符，不剥会把中文释义判成 en
        let definitions: Vec<String> = (0..10)
            .map(|i| format!("<div class=\"w\">&nbsp;&amp;</div>这是一段中文释义内容{i}</div>"))
            .collect();
        let headwords: Vec<String> = (0..10).map(|i| format!("englishword{i}")).collect();
        let (from, to) = detect_language(&headwords, &definitions);
        assert_eq!(from, Some("en"));
        assert_eq!(to, Some("zh-Hans"));
    }

    #[test]
    fn sparse_kana_in_chinese_dict_not_ja() {
        // 中文释义里零星假名（kana*3 < cjk）不应判成 ja
        let words: Vec<&str> = ["中", "国", "语", "言", "学", "习", "词", "典", "あ", "い", "う", "え", "お", "汉", "字", "测", "试", "数", "据", "样", "本"]
            .to_vec();
        let (from, _) = detect(&words, &[]);
        assert_eq!(from, Some("zh-Hans"));
    }
}
