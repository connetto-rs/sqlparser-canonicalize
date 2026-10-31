use std::collections::BTreeMap;

use sqlparser::dialect::{MySqlDialect, PostgreSqlDialect, SQLiteDialect};
use sqlparser_canonicalize::{Canonicalizer, hash_canonical};

#[derive(Clone, Copy, Debug)]
enum DialectKind {
    PostgreSql,
    MySql,
    SQLite,
}

#[derive(Debug)]
struct Golden {
    dialect: DialectKind,
    sql: &'static str,
    normalized: &'static str,
    hash: u128,
}

const GOLDENS: &[Golden] = &[
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t",
        normalized: "true",
        hash: 196429622312853751706263382814465376604,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE age > 18",
        normalized: "(18 < age)",
        hash: 316111075571651616950055409650343842010,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE age >= 18",
        normalized: "(18 <= age)",
        hash: 278432499478339973164059801902769922318,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE age < 65",
        normalized: "(65 > age)",
        hash: 83492278720086946753790729493557862069,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE age <= 65",
        normalized: "(65 >= age)",
        hash: 244161918419377154061829666031752459250,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE age = 42",
        normalized: "(42 = age)",
        hash: 302037566819660740417919068379990631044,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE age != 42",
        normalized: "(42 != age)",
        hash: 102968402348262718566844936109611287537,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE deleted_at IS NOT DISTINCT FROM NULL",
        normalized: "(NULL IS NOT DISTINCT FROM deleted_at)",
        hash: 99315443687852918114510564172484027840,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE status IS DISTINCT FROM 'active'",
        normalized: "('active' IS DISTINCT FROM status)",
        hash: 176171465578505115639774219110214302167,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE a = 1 AND b = 2",
        normalized: "((1 = a) AND (2 = b))",
        hash: 131670881900975508605752108541534450950,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE b = 2 AND a = 1",
        normalized: "((1 = a) AND (2 = b))",
        hash: 131670881900975508605752108541534450950,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE a = 1 OR b = 2",
        normalized: "((1 = a) OR (2 = b))",
        hash: 295580346008773016158339669541369738333,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE b = 2 OR a = 1",
        normalized: "((1 = a) OR (2 = b))",
        hash: 295580346008773016158339669541369738333,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE (a = 1 AND b = 2) AND c = 3",
        normalized: "(((1 = a) AND (2 = b)) AND (3 = c))",
        hash: 182759154872355133982334454209519610451,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE a = 1 AND (b = 2 AND c = 3)",
        normalized: "(((1 = a) AND (2 = b)) AND (3 = c))",
        hash: 182759154872355133982334454209519610451,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE (a = 1 OR b = 2) OR c = 3",
        normalized: "(((1 = a) OR (2 = b)) OR (3 = c))",
        hash: 95758464856935287648057063807289397135,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE a = 1 OR (b = 2 OR c = 3)",
        normalized: "(((1 = a) OR (2 = b)) OR (3 = c))",
        hash: 95758464856935287648057063807289397135,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE x IN (3, 1, 2)",
        normalized: "x IN (1, 2, 3)",
        hash: 235230893105783445659199798622162550252,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE x NOT IN (3, 1, 2)",
        normalized: "x NOT IN (1, 2, 3)",
        hash: 277416314403838883454401070234615665757,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE ((age > 18))",
        normalized: "(18 < age)",
        hash: 316111075571651616950055409650343842010,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE a < b",
        normalized: "(a < b)",
        hash: 14746718521301734887795259058823844396,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE b < a",
        normalized: "(a > b)",
        hash: 204201253671108966590160388409485334977,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE a + b > 10",
        normalized: "((a + b) > 10)",
        hash: 2811918157887371711831872217975099545,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE a - b > 10",
        normalized: "((a - b) > 10)",
        hash: 74443246094558125713561247511123700650,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE a * b > 10",
        normalized: "((a * b) > 10)",
        hash: 311378054065053570425432075285704523444,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE a / b > 10",
        normalized: "((a / b) > 10)",
        hash: 291742834309287946499546270378476167205,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE a % b > 10",
        normalized: "((a % b) > 10)",
        hash: 82868508035341089427090561311991876240,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE NOT (active = TRUE)",
        normalized: "NOT (active = true)",
        hash: 65851817300140767743482816604299764526,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE deleted_at IS NULL",
        normalized: "deleted_at IS NULL",
        hash: 260105666492253831600832914225129674266,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE deleted_at IS NOT NULL",
        normalized: "deleted_at IS NOT NULL",
        hash: 332224121843328788158539955817329118924,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE name LIKE 'A%'",
        normalized: "name LIKE 'A%'",
        hash: 314974045665179851130112309972384705706,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE name NOT LIKE 'A%'",
        normalized: "name NOT LIKE 'A%'",
        hash: 94303378871260595433799648522321568991,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE age BETWEEN 18 AND 65",
        normalized: "age BETWEEN 18 AND 65",
        hash: 226023245311690468322616306060892911446,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE age NOT BETWEEN 18 AND 65",
        normalized: "age NOT BETWEEN 18 AND 65",
        hash: 114231437004172268307254474201086575722,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE status = 'paid'",
        normalized: "('paid' = status)",
        hash: 10488038035579433165683966987072516130,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE status = 'open' AND amount > 100",
        normalized: "(('open' = status) AND (100 < amount))",
        hash: 188608718948517086269448025140156137829,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE x IN (SELECT id FROM m WHERE owner = 'a')",
        normalized: "x IN (SELECT id FROM m WHERE ('a' = owner))",
        hash: 278058222354865618142828421867605410868,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE x NOT IN (SELECT id FROM m WHERE owner = 'a')",
        normalized: "x NOT IN (SELECT id FROM m WHERE ('a' = owner))",
        hash: 101089862008503157561429321991088829490,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT COUNT(*) FROM t",
        normalized: "true",
        hash: 196429622312853751706263382814465376604,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT SUM(amount) FROM t WHERE amount > 10",
        normalized: "(10 < amount)",
        hash: 244853546457191983353911351810811010689,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT status, COUNT(*) FROM t WHERE active = TRUE GROUP BY status",
        normalized: "(active = true)",
        hash: 61822439498332721400316582865907409867,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT region, SUM(amount) FROM orders GROUP BY region HAVING SUM(amount) > 10",
        normalized: "true",
        hash: 196429622312853751706263382814465376604,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT region, SUM(amount) FROM orders WHERE active = TRUE GROUP BY region HAVING COUNT(*) > 2",
        normalized: "(active = true)",
        hash: 61822439498332721400316582865907409867,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE score = -5",
        normalized: "(- 5 = score)",
        hash: 263991339775984971083937534105788456146,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM orders WHERE \"Status\" = 'paid'",
        normalized: "(\"Status\" = 'paid')",
        hash: 244578590542428046936922079652582969739,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t",
        normalized: "true",
        hash: 196429622312853751706263382814465376604,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE age > 18",
        normalized: "(18 < age)",
        hash: 316111075571651616950055409650343842010,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE age >= 18",
        normalized: "(18 <= age)",
        hash: 278432499478339973164059801902769922318,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE age < 65",
        normalized: "(65 > age)",
        hash: 83492278720086946753790729493557862069,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE age <= 65",
        normalized: "(65 >= age)",
        hash: 244161918419377154061829666031752459250,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE age = 42",
        normalized: "(42 = age)",
        hash: 302037566819660740417919068379990631044,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE age != 42",
        normalized: "(42 != age)",
        hash: 102968402348262718566844936109611287537,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE deleted_at <=> NULL",
        normalized: "(NULL <=> deleted_at)",
        hash: 334564663628246787903199153241645203912,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE a = 1 AND b = 2",
        normalized: "((1 = a) AND (2 = b))",
        hash: 131670881900975508605752108541534450950,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE b = 2 AND a = 1",
        normalized: "((1 = a) AND (2 = b))",
        hash: 131670881900975508605752108541534450950,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE a = 1 OR b = 2",
        normalized: "((1 = a) OR (2 = b))",
        hash: 295580346008773016158339669541369738333,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE b = 2 OR a = 1",
        normalized: "((1 = a) OR (2 = b))",
        hash: 295580346008773016158339669541369738333,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE (a = 1 AND b = 2) AND c = 3",
        normalized: "(((1 = a) AND (2 = b)) AND (3 = c))",
        hash: 182759154872355133982334454209519610451,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE a = 1 AND (b = 2 AND c = 3)",
        normalized: "(((1 = a) AND (2 = b)) AND (3 = c))",
        hash: 182759154872355133982334454209519610451,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE (a = 1 OR b = 2) OR c = 3",
        normalized: "(((1 = a) OR (2 = b)) OR (3 = c))",
        hash: 95758464856935287648057063807289397135,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE a = 1 OR (b = 2 OR c = 3)",
        normalized: "(((1 = a) OR (2 = b)) OR (3 = c))",
        hash: 95758464856935287648057063807289397135,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE x IN (3, 1, 2)",
        normalized: "x IN (1, 2, 3)",
        hash: 235230893105783445659199798622162550252,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE x NOT IN (3, 1, 2)",
        normalized: "x NOT IN (1, 2, 3)",
        hash: 277416314403838883454401070234615665757,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE ((age > 18))",
        normalized: "(18 < age)",
        hash: 316111075571651616950055409650343842010,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE a < b",
        normalized: "(a < b)",
        hash: 14746718521301734887795259058823844396,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE b < a",
        normalized: "(a > b)",
        hash: 204201253671108966590160388409485334977,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE a + b > 10",
        normalized: "((a + b) > 10)",
        hash: 2811918157887371711831872217975099545,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE a - b > 10",
        normalized: "((a - b) > 10)",
        hash: 74443246094558125713561247511123700650,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE a * b > 10",
        normalized: "((a * b) > 10)",
        hash: 311378054065053570425432075285704523444,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE a / b > 10",
        normalized: "((a / b) > 10)",
        hash: 291742834309287946499546270378476167205,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE a % b > 10",
        normalized: "((a % b) > 10)",
        hash: 82868508035341089427090561311991876240,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE NOT (active = TRUE)",
        normalized: "NOT (active = true)",
        hash: 65851817300140767743482816604299764526,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE deleted_at IS NULL",
        normalized: "deleted_at IS NULL",
        hash: 260105666492253831600832914225129674266,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE deleted_at IS NOT NULL",
        normalized: "deleted_at IS NOT NULL",
        hash: 332224121843328788158539955817329118924,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE name LIKE 'A%'",
        normalized: "name LIKE 'A%'",
        hash: 314974045665179851130112309972384705706,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE name NOT LIKE 'A%'",
        normalized: "name NOT LIKE 'A%'",
        hash: 94303378871260595433799648522321568991,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE age BETWEEN 18 AND 65",
        normalized: "age BETWEEN 18 AND 65",
        hash: 226023245311690468322616306060892911446,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE age NOT BETWEEN 18 AND 65",
        normalized: "age NOT BETWEEN 18 AND 65",
        hash: 114231437004172268307254474201086575722,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE status = 'paid'",
        normalized: "('paid' = status)",
        hash: 10488038035579433165683966987072516130,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE status = 'open' AND amount > 100",
        normalized: "(('open' = status) AND (100 < amount))",
        hash: 188608718948517086269448025140156137829,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE x IN (SELECT id FROM m WHERE owner = 'a')",
        normalized: "x IN (SELECT id FROM m WHERE ('a' = owner))",
        hash: 278058222354865618142828421867605410868,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE x NOT IN (SELECT id FROM m WHERE owner = 'a')",
        normalized: "x NOT IN (SELECT id FROM m WHERE ('a' = owner))",
        hash: 101089862008503157561429321991088829490,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT COUNT(*) FROM t",
        normalized: "true",
        hash: 196429622312853751706263382814465376604,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT SUM(amount) FROM t WHERE amount > 10",
        normalized: "(10 < amount)",
        hash: 244853546457191983353911351810811010689,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT status, COUNT(*) FROM t WHERE active = TRUE GROUP BY status",
        normalized: "(active = true)",
        hash: 61822439498332721400316582865907409867,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT region, SUM(amount) FROM orders GROUP BY region HAVING SUM(amount) > 10",
        normalized: "true",
        hash: 196429622312853751706263382814465376604,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT region, SUM(amount) FROM orders WHERE active = TRUE GROUP BY region HAVING COUNT(*) > 2",
        normalized: "(active = true)",
        hash: 61822439498332721400316582865907409867,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE score = -5",
        normalized: "(- 5 = score)",
        hash: 263991339775984971083937534105788456146,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM orders WHERE `Status` = 'paid'",
        normalized: "('paid' = `status`)",
        hash: 177394572371791273191020032091017557400,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t",
        normalized: "(1 = 1)",
        hash: 29413222210268323713216642985668275023,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE age > 18",
        normalized: "(18 < age)",
        hash: 316111075571651616950055409650343842010,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE age >= 18",
        normalized: "(18 <= age)",
        hash: 278432499478339973164059801902769922318,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE age < 65",
        normalized: "(65 > age)",
        hash: 83492278720086946753790729493557862069,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE age <= 65",
        normalized: "(65 >= age)",
        hash: 244161918419377154061829666031752459250,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE age = 42",
        normalized: "(42 = age)",
        hash: 302037566819660740417919068379990631044,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE age != 42",
        normalized: "(42 != age)",
        hash: 102968402348262718566844936109611287537,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE deleted_at IS NOT DISTINCT FROM NULL",
        normalized: "(NULL IS NOT DISTINCT FROM deleted_at)",
        hash: 99315443687852918114510564172484027840,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE status IS DISTINCT FROM 'active'",
        normalized: "('active' IS DISTINCT FROM status)",
        hash: 176171465578505115639774219110214302167,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE a = 1 AND b = 2",
        normalized: "((1 = a) AND (2 = b))",
        hash: 131670881900975508605752108541534450950,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE b = 2 AND a = 1",
        normalized: "((1 = a) AND (2 = b))",
        hash: 131670881900975508605752108541534450950,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE a = 1 OR b = 2",
        normalized: "((1 = a) OR (2 = b))",
        hash: 295580346008773016158339669541369738333,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE b = 2 OR a = 1",
        normalized: "((1 = a) OR (2 = b))",
        hash: 295580346008773016158339669541369738333,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE (a = 1 AND b = 2) AND c = 3",
        normalized: "(((1 = a) AND (2 = b)) AND (3 = c))",
        hash: 182759154872355133982334454209519610451,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE a = 1 AND (b = 2 AND c = 3)",
        normalized: "(((1 = a) AND (2 = b)) AND (3 = c))",
        hash: 182759154872355133982334454209519610451,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE (a = 1 OR b = 2) OR c = 3",
        normalized: "(((1 = a) OR (2 = b)) OR (3 = c))",
        hash: 95758464856935287648057063807289397135,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE a = 1 OR (b = 2 OR c = 3)",
        normalized: "(((1 = a) OR (2 = b)) OR (3 = c))",
        hash: 95758464856935287648057063807289397135,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE x IN (3, 1, 2)",
        normalized: "x IN (1, 2, 3)",
        hash: 235230893105783445659199798622162550252,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE x NOT IN (3, 1, 2)",
        normalized: "x NOT IN (1, 2, 3)",
        hash: 277416314403838883454401070234615665757,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE ((age > 18))",
        normalized: "(18 < age)",
        hash: 316111075571651616950055409650343842010,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE a < b",
        normalized: "(a < b)",
        hash: 14746718521301734887795259058823844396,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE b < a",
        normalized: "(b < a)",
        hash: 164195086123485964968356541696287096375,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE a + b > 10",
        normalized: "((a + b) > 10)",
        hash: 2811918157887371711831872217975099545,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE a - b > 10",
        normalized: "((a - b) > 10)",
        hash: 74443246094558125713561247511123700650,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE a * b > 10",
        normalized: "((a * b) > 10)",
        hash: 311378054065053570425432075285704523444,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE a / b > 10",
        normalized: "((a / b) > 10)",
        hash: 291742834309287946499546270378476167205,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE a % b > 10",
        normalized: "((a % b) > 10)",
        hash: 82868508035341089427090561311991876240,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE NOT (active = TRUE)",
        normalized: "NOT (active = true)",
        hash: 65851817300140767743482816604299764526,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE deleted_at IS NULL",
        normalized: "deleted_at IS NULL",
        hash: 260105666492253831600832914225129674266,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE deleted_at IS NOT NULL",
        normalized: "deleted_at IS NOT NULL",
        hash: 332224121843328788158539955817329118924,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE name LIKE 'A%'",
        normalized: "name LIKE 'A%'",
        hash: 314974045665179851130112309972384705706,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE name NOT LIKE 'A%'",
        normalized: "name NOT LIKE 'A%'",
        hash: 94303378871260595433799648522321568991,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE age BETWEEN 18 AND 65",
        normalized: "age BETWEEN 18 AND 65",
        hash: 226023245311690468322616306060892911446,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE age NOT BETWEEN 18 AND 65",
        normalized: "age NOT BETWEEN 18 AND 65",
        hash: 114231437004172268307254474201086575722,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE status = 'paid'",
        normalized: "('paid' = status)",
        hash: 10488038035579433165683966987072516130,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE status = 'open' AND amount > 100",
        normalized: "(('open' = status) AND (100 < amount))",
        hash: 188608718948517086269448025140156137829,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE x IN (SELECT id FROM m WHERE owner = 'a')",
        normalized: "x IN (SELECT id FROM m WHERE ('a' = owner))",
        hash: 278058222354865618142828421867605410868,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE x NOT IN (SELECT id FROM m WHERE owner = 'a')",
        normalized: "x NOT IN (SELECT id FROM m WHERE ('a' = owner))",
        hash: 101089862008503157561429321991088829490,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT COUNT(*) FROM t",
        normalized: "(1 = 1)",
        hash: 29413222210268323713216642985668275023,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT SUM(amount) FROM t WHERE amount > 10",
        normalized: "(10 < amount)",
        hash: 244853546457191983353911351810811010689,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT status, COUNT(*) FROM t WHERE active = TRUE GROUP BY status",
        normalized: "(active = true)",
        hash: 61822439498332721400316582865907409867,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT region, SUM(amount) FROM orders GROUP BY region HAVING SUM(amount) > 10",
        normalized: "(1 = 1)",
        hash: 29413222210268323713216642985668275023,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT region, SUM(amount) FROM orders WHERE active = TRUE GROUP BY region HAVING COUNT(*) > 2",
        normalized: "(active = true)",
        hash: 61822439498332721400316582865907409867,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE score = -5",
        normalized: "(- 5 = score)",
        hash: 263991339775984971083937534105788456146,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM orders WHERE \"Status\" = 'paid'",
        normalized: "(\"status\" = 'paid')",
        hash: 112717352067792148743778502094369901298,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE Active IS NOT TRUE",
        normalized: "active IS NOT TRUE",
        hash: 169325908433798865918380666061595783232,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE Name SIMILAR TO 'a%'",
        normalized: "name SIMILAR TO 'a%'",
        hash: 293999106747452751383688411230001914370,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE Id = SOME(Ids)",
        normalized: "id = ANY(ids)",
        hash: 252311792306329106316691031148903498536,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE Name REGEXP '^a'",
        normalized: "name RLIKE '^a'",
        hash: 191561548683398552017483930128739763238,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE 3 MEMBER OF(Tags)",
        normalized: "3 MEMBER OF(tags)",
        hash: 177081032619041551905878422141399186199,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE Flag IS UNKNOWN",
        normalized: "(flag IS NOT DISTINCT FROM unknown)",
        hash: 146294889612496539165456862934178885956,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE LOWER(Email) = 'a@b.c'",
        normalized: "('a@b.c' = lower(email))",
        hash: 252573738667588764506528454302052192055,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE Amount::NUMERIC > 10",
        normalized: "(10 < CAST(amount AS NUMERIC))",
        hash: 171201879322019007818016109927481221327,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE CASE WHEN Paid THEN 1 ELSE NULL END = 1",
        normalized: "(1 = CASE WHEN paid THEN 1 END)",
        hash: 302285422036760154290977927312778686673,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE EXTRACT(YEAR FROM Created) = 2026",
        normalized: "(2026 = EXTRACT(YEAR FROM created))",
        hash: 154063077115065842130430037709384758435,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE CONVERT(Name USING utf8mb4) = 'x'",
        normalized: "('x' = CONVERT(name USING utf8mb4))",
        hash: 73960171852098668834036988794385022741,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE SUBSTR(Code, 1, 2) = 'ab'",
        normalized: "('ab' = SUBSTR(code, 1, 2))",
        hash: 69633333979695294109422699580078467064,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE EXISTS (SELECT * FROM Orders WHERE Orders.Owner = t.Id)",
        normalized: "EXISTS (SELECT * FROM orders WHERE (orders.owner = t.id))",
        hash: 225956965931835869503995965403457904073,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE Total > (SELECT AVG(Total) FROM Orders)",
        normalized: "((SELECT avg(total) FROM Orders) < total)",
        hash: 63845607836025529440545921617897993232,
    },
    Golden {
        dialect: DialectKind::SQLite,
        sql: "SELECT * FROM t WHERE NOT EXISTS (SELECT 1 FROM Orders WHERE Owner = Id)",
        normalized: "NOT EXISTS (SELECT 1 FROM orders WHERE (owner = id))",
        hash: 36450148689480009697569547122790864796,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM docs WHERE EXISTS (SELECT 1 FROM Shares AS S WHERE S.doc_id = docs.id)",
        normalized: "EXISTS (SELECT 1 FROM shares s WHERE (docs.id = s.doc_id))",
        hash: 14807432777728440388075203346013765724,
    },
    Golden {
        dialect: DialectKind::MySql,
        sql: "SELECT * FROM t WHERE x IN (SELECT a FROM m AS V)",
        normalized: "x IN (SELECT a FROM m V)",
        hash: 273400758536552858398955164991264260047,
    },
    Golden {
        dialect: DialectKind::PostgreSql,
        sql: "SELECT * FROM t WHERE amount IN (1, ((SELECT amount FROM orders)))",
        normalized: "amount IN (1, (SELECT amount FROM orders))",
        hash: 272223159844162993213568602930665551268,
    },
];

fn normalize_with_dialect(sql: &str, dialect: DialectKind) -> String {
    match dialect {
        DialectKind::PostgreSql => Canonicalizer::new(&PostgreSqlDialect {}).normalize_sql(sql),
        DialectKind::MySql => Canonicalizer::new(&MySqlDialect {}).normalize_sql(sql),
        DialectKind::SQLite => Canonicalizer::new(&SQLiteDialect {}).normalize_sql(sql),
    }
    .unwrap()
}

#[test]
fn matches_subql_compatibility_surface() {
    assert!(GOLDENS.len() >= 100);
    for golden in GOLDENS {
        let normalized = normalize_with_dialect(golden.sql, golden.dialect);
        assert_eq!(normalized, golden.normalized, "SQL: {}", golden.sql);
        assert_eq!(
            hash_canonical(&normalized),
            golden.hash,
            "SQL: {}",
            golden.sql
        );
    }
}

#[test]
fn distinct_golden_text_has_distinct_hashes() {
    let mut normalized_by_hash = BTreeMap::new();
    for golden in GOLDENS {
        if let Some(previous) = normalized_by_hash.insert(golden.hash, golden.normalized) {
            assert_eq!(previous, golden.normalized, "hash: {}", golden.hash);
        }
    }
}

#[test]
fn golden_canonical_forms_are_idempotent() {
    for golden in GOLDENS {
        let sql = format!("SELECT * FROM t WHERE {}", golden.normalized);
        let second = normalize_with_dialect(&sql, golden.dialect);
        assert_eq!(second, golden.normalized, "SQL: {}", golden.sql);
    }
}
