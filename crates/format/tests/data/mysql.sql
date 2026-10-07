-- Scripts as people write them, for the formatter's tests.
create database if not exists shop;
use shop;
create table `orders` (`id` int unsigned not null auto_increment, `user_id` int not null, `total` decimal(10,2) not null default '0.00', `status` enum('new','paid') not null default 'new', `note` text comment 'Free text', `created` datetime not null default current_timestamp on update current_timestamp, primary key (`id`), key `orders_user` (`user_id`), constraint `orders_user_fk` foreign key (`user_id`) references `users` (`id`) on delete cascade) engine=InnoDB auto_increment=10 default charset=utf8mb4 collate=utf8mb4_unicode_ci comment='Orders';
insert into orders (user_id, total) values (1, 9.95), (2, 1.00) on duplicate key update total = values(total), status = 'new';
insert into orders set user_id = 3, total = 2;
replace into orders (id, user_id) values (1, 1);
update orders o join users u on u.id = o.user_id set o.status = 'paid', u.name = 'x' where o.total > 0 order by o.id limit 5;
delete o from orders o join users u on u.id = o.user_id where u.name is null;
select sql_calc_found_rows id, user_id, total, if(total > 10, 'big', 'small') as size, date_add(created, interval 1 day), group_concat(note order by id separator ', '), json_extract(note, '$.a'), note->'$.b', note->>'$.c', @rank := @rank + 1, @@session.sql_mode from orders force index (orders_user) where status = 'new' and match (note) against ('word' in boolean mode) group by user_id with rollup having count(*) > 1 order by total desc limit 10, 20 for update;
select * from orders where id in (1, 2, 3) union distinct select * from archive order by id limit 3;
set @rank = 0, @@session.sql_mode = 'ANSI';
DELIMITER //
create procedure shop.archive(in days int, out moved int)
begin
  declare done int default false;
  declare cur cursor for select id from orders where created < now() - interval days day;
  declare continue handler for not found set done = true;
  open cur;
  read_loop: loop
    fetch cur into moved;
    if done then
      leave read_loop;
    end if;
  end loop;
  close cur;
  repeat set moved = moved - 1; until moved <= 0 end repeat;
  case days when 1 then select 'one'; else select 'many'; end case;
end //
create trigger orders_touch before insert on orders for each row begin set new.created = now(); end//
create function shop.double_it(x int) returns int deterministic return x * 2//
DELIMITER ;
select _utf8mb4'abc' collate utf8mb4_bin, b'101', x'ff', 0x1f, 1e3, .5, 'it\'s';
show tables;
alter table orders add index (status), modify column note varchar(255), change column total amount decimal(12,2), drop primary key;
rename table orders to orders_old, archive to orders;
grant select on shop.* to 'reader'@'localhost';
