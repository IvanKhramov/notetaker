# Индекс 0.24.1

index-0.24.1.sqlite создан rusqlite 0.38.0 с bundled libsqlite3-sys 0.36.0.
Использованы DDL из connect_index версии 0.24.1 (commit 45757c9),
PRAGMA user_version=4 и по одной синтетической записи во всех пяти таблицах.
FTS содержит слова compatibility token, vector содержит float32 1.0.
Файл не содержит пользовательских данных.

Тест opens_0241_index_without_rebuilding_or_changing_schema открывает копию
новой библиотекой, сравнивает sqlite_master, проверяет записи, FTS и вектор,
записывает данные и повторно открывает базу.
