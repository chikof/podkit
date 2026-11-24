DELETE FROM role_permissions WHERE permission_id IN (SELECT id FROM permissions WHERE resource = 'model_file');
DELETE FROM permissions WHERE resource = 'model_file';
DROP TABLE IF EXISTS model_files;
DROP TABLE IF EXISTS huggingface_tokens;
