set search_path to app;

create view special_forms as
select coalesce(name, 'none') as n, nullif(id, 0) as i, greatest(a, b) as g
from t;
