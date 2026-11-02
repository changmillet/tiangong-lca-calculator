# Study service seam observation

`database-service-seams.json` is a sanitized actual PostgreSQL17 `pg_proc`
observation from the task-owned Study894 project. Database #895 exported it
against the exact W03/W06 source hashes in the fixture. It contains function
names, ordered arguments, result types and service/actor ACL booleans, with no
actor records, source documents, tokens or storage credentials.

Worker tests compare its actual read/completion dispatch map to these producer
seams. Source recording is `svc_study_source_record_v1`; naming symmetry does
not authorize an invented `source_acquisition_complete` function. Refresh the
observation when Database changes these seams and verify the actual integrated
runtime separately. This catalog observation is not hosted deployment or full
end-to-end qualification.
