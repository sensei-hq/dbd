DO $$ BEGIN
  IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'app_ro') THEN
    CREATE ROLE "app_ro";
  END IF;
END $$;
GRANT "app_admin" TO "app_ro";
