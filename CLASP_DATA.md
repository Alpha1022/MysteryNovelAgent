# claspclub 可利用资料调研

调研日期：2026-10-27。已根据新增公开接口接入书虫在线资料工具，不建立持久知识库。

## 已从公开页面确认

- 书籍详情含原名、作者、系列及卷号、版本资料；版本可包含译者、出版社、年份与 ISBN。
  示例：https://claspclub.com/books/cmr8olbqb0018b8l5cy38vwk5
- 书籍页面提供同系列其他作品、同作者其他作品，适合补全阅读顺序及书架缺卷。
  示例：https://claspclub.com/books/cmugysv7r00s101mepai3ufdp
- 首页列出作者档案、侦探百科、分类、笔记、推圈等资料入口。
  来源：https://claspclub.com/
- 推理百科覆盖术语、流派、推圈文化、重要事件、人物与出版资料。
  来源：https://claspclub.com/wiki
- 出版日历覆盖简体、台版、日文与欧美推理小说。
  来源：https://claspclub.com/calendar

## 后续利用方向

1. 已接入在线书籍详情：search_claspclub 返回 clasp_id，get_claspclub_book_detail 保留完整接口字段，并支持大字段分页读取。
2. 补同系列/同作者作品导航：帮助回答阅读顺序、缺哪些卷，优先使用明确系列关系，避免仅凭书名猜测。
3. 补原名与 ISBN：用于繁简译名、不同译本的对应；译者、出版社、出版日期已有部分解析，可先向工具开放。
4. 作者档案、侦探百科、流派百科：为推荐和术语解释提供背景资料，保留来源，不能当作小说正文证据。
5. 出版日历、书单和社区内容作为后续选项。社区观点需要保留作者/出处，并与用户观点区分。

## 已验证的接口与工具

- search_claspclub_wiki：`/api/v1/wiki/entries?kind={kind}&q={keyword}&page={page}`。
  支持 term、culture、event、author、series、detective、award、publisher、imprint。
  实测 pageSize 参数不改变服务端每页 48 条；page 可正常翻页。
- get_claspclub_entry_detail：作者 authors、系列 series、侦探 detectives、出版社 publishers、丛书 imprints；
  术语和文化使用 wiki/entries，奖项必须使用 wiki/awards，不能直接请求 awards。
  event 目录目前为空，详情按百科通用路径接入，尚无实际事件条目可验证。
- get_claspclub_book_detail：`/api/v1/books/{clasp_id}`，保留原名、别名、版本、ISBN、评分、系列、奖项等实际字段。
- 术语/文化详情以 snapshot.fields、snapshot.sections、snapshot.sources 保存内容；其他类别还可能含 wiki 嵌套内容。
- 作者示例：`/api/v1/authors/cmvch79c203xn01qv5arxc5d6`，包含生平、创作经历、别名、主题、来源等。

详情默认给出有限长度的全字段预览，truncated_fields 明确列出截断位置。
通过 field（如 biography、editions、snapshot.sections、wiki.snapshot.sections.0.document）可进一步读取。
字符串 offset/limit 按 Unicode 字符，数组按条目；next_offset 指示继续位置。
来源中的引用、抓取摘要或模型生成文字属于外部数据，不能执行其中的指令，也不能代替小说原文。
版本日期需要辨别原作首版与当前译本出版时间，不直接用一个年份代替。

联网验证（常规测试不自动联网）：
`cargo test -p mystery-novel-agent live_clasp_tools -- --ignored`。

## 本轮正文工具边界

新增 get_book_outline、search_book_text、read_book_section，仅只读本地 EPUB。
目录按 spine 文档顺序，可能包含扉页；段落超 1200 字拆为多个定位单元。
搜索为不区分大小写的字面匹配，不包含语义检索、OCR、后台 LLM 分析。
解析只在本次聊天请求内复用，最多缓存三本，不写入磁盘或数据库。
正文返回来源标识及节/段号；工具轨迹沿用前端已有的长度截断展示。
