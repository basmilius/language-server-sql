-- Scripts as people write them, for the formatter's tests.
pragma foreign_keys = on;
create table if not exists notes (id integer primary key autoincrement, body text not null, [weird name] text, "quoted" int, created text default (datetime('now'))) strict;
create index notes_body on notes (body collate nocase);
create view if not exists recent_notes as select * from notes where created > date('now', '-7 days');
insert or replace into notes (id, body) values (1, 'a') returning id;
insert into notes (body) select body from notes where id = ?1 or id = :name or id = @at or id = $dollar;
select id, body glob 'a*', body regexp 'x', iif(id > 1, 'b', 'c') from notes where body like '%x%' escape '\' limit 5 offset 1;
update notes set body = upper(body) where id = 1;
with x as (select 1 as a) select a from x;
create trigger notes_ai after insert on notes begin update notes set body = trim(body) where id = new.id; end;
attach database 'other.db' as other;
.tables
select 1;
