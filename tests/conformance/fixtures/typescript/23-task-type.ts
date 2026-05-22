import { Pipeline } from '../../../../sdk/typescript/src/index';

const p = new Pipeline();

p.task('build').run('echo build').taskType('build');
p.task('test').run('echo test').taskType('test').after('build');
p.task('lint').run('echo lint').taskType('lint').after('test');
p.task('format').run('echo format').taskType('format').after('lint');
p.task('scan').run('echo scan').taskType('scan').after('format');
p.task('package').run('echo package').taskType('package').after('scan');
p.task('publish').run('echo publish').taskType('publish').after('package');
p.task('deploy').run('echo deploy').taskType('deploy').after('publish');
p.task('migrate').run('echo migrate').taskType('migrate').after('deploy');
p.task('generate').run('echo generate').taskType('generate').after('migrate');
p.task('verify').run('echo verify').taskType('verify').after('generate');
p.task('cleanup').run('echo cleanup').taskType('cleanup').after('verify');

p.emit();
