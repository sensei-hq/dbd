set search_path to app;

create view joins_and_cte as
with recent as (select id from t where id > 100)
select r.id, u.email
from recent r
join shop.users u on u.id = r.id;
