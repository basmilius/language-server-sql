-- Scripts as people write them, for the formatter's tests: every one formats to the same tokens,
-- and formatting the result again changes nothing.
create schema if not exists app;
set search_path to app, public;
create type mood as enum ('happy', 'sad');
create domain email as text check (value ~ '^[^@]+@[^@]+$');
create sequence order_numbers start 100 increment 1;
create table app.users (id bigint generated always as identity primary key, email email not null unique, name text, mood mood default 'happy', created timestamp with time zone not null default now(), tags text[] default '{}', data jsonb);
comment on table app.users is 'People';
comment on column app.users.email is 'Where mail goes';
create unique index concurrently if not exists users_lower_email on app.users (lower(email)) where name is not null;
create materialized view app.recent as select id, email from app.users where created > now() - interval '7 days' with data;
create or replace function app.add(a integer, b integer default 1) returns integer language sql immutable as $$ select a + b $$;
create function app.touch() returns trigger language plpgsql as $body$
begin
  new.created := now();
  return new;
end;
$body$;
create trigger users_touch before update on app.users for each row execute function app.touch();
select id, email, data->>'name' as name, data#>'{a,b}' as path, tags[1], tags[1:2], created::date, extract(year from created) as year, coalesce(name, 'none'), case mood when 'happy' then 1 else 0 end from app.users where email ilike '%@example.com' and id = any(array[1, 2, 3]) and data @> '{"active": true}' order by created desc nulls last fetch first 10 rows only;
select distinct on (email) email, id from app.users order by email, id;
select u.id, count(o.*) filter (where o.total > 0) as paid, string_agg(o.note, ', ' order by o.id) within group (order by o.id), rank() over w from app.users u left join lateral (select * from orders o where o.user_id = u.id limit 3) o on true cross join generate_series(1, 3) as g(n) group by grouping sets ((u.id), ()) window w as (partition by u.id order by u.created rows between unbounded preceding and current row);
with recursive tree (id, parent, depth) as (select id, parent, 0 from nodes where parent is null union all select n.id, n.parent, t.depth + 1 from nodes n join tree t on n.parent = t.id) search depth first by id set ordercol select * from tree;
insert into app.users (email, name) values ('a@b.c', 'A') on conflict (email) do nothing;
insert into app.users as u (email) select email from staging s where not exists (select 1 from app.users x where x.email = s.email) returning u.id, u.email;
update app.users u set name = s.name, mood = 'sad' from staging s where s.email = u.email and s.name is distinct from u.name;
delete from app.users where id in (select id from app.users order by created limit 10) returning *;
merge into app.users u using staging s on s.email = u.email when matched and s.name is null then delete when matched then update set name = s.name when not matched then insert (email, name) values (s.email, s.name);
select 1 where 1 between 0 and 2 and 'a' similar to 'a%' and 3 not in (1, 2) and $1::int > -1 and - -1 = 1;
select (select max(id) from app.users) as top, exists (select 1) as has, array(select id from app.users) as ids;
do $$ begin raise notice 'hi'; end $$;
values (1, 'a'), (2, 'b');
table app.users;
explain analyze select * from app.users;
begin;
lock table app.users in exclusive mode;
commit;
alter table app.users add column age int check (age > 0), alter column name set not null, rename column data to payload;
alter table app.users rename to people;
drop table if exists app.old cascade;
grant select, insert on app.users to reader;
copy app.users (id, email) from stdin with (format csv);
1,a@b.c
\.
\echo done
