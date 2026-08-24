set search_path to app;

create type dollar_quoted_comment as enum ('a', 'b');
comment on type dollar_quoted_comment is $c$a label's note$c$;
