-- Пустой файл (.gitkeep, пустой журнал, заглушка) — законное вложение и законный файл агента.
-- Картинки агента (block_media) остаются непустыми: пустой файл не бывает картинкой.
alter table attachments drop constraint attachments_size_bytes_check,
  add constraint attachments_size_bytes_check check (size_bytes between 0 and 104857600);
alter table block_files drop constraint block_files_size_bytes_check,
  add constraint block_files_size_bytes_check check (size_bytes between 0 and 52428800);
