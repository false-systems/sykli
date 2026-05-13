import { Pipeline } from '../../../../sdk/typescript/src/index';

const p = new Pipeline();
p.task('test')
  .run('go test ./...')
  .taskType('test')
  .evidenceRequired([
    {
      type: 'file',
      name: 'coverage',
      required: true,
      visibility: 'local',
      predicate: 'non_empty',
      ref_pattern: 'coverage.out',
    },
  ]);
p.emit();
