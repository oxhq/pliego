<?php

declare(strict_types=1);

namespace Pliego\Laravel\Console;

use Illuminate\Console\Command;
use InvalidArgumentException;
use Pliego\Php\JobRetention;
use RuntimeException;

final class PruneCommand extends Command
{
    protected $signature = 'pliego:prune {--dry-run : Report eligible jobs without deleting them}';

    protected $description = 'Delete expired retained Pliego jobs';

    public function handle(): int
    {
        try {
            $result = (new JobRetention())->prune(
                (string) config('pliego.work_dir'),
                (int) config('pliego.success_retention_seconds'),
                (int) config('pliego.failure_retention_seconds'),
                (bool) $this->option('dry-run'),
            );
        } catch (InvalidArgumentException|RuntimeException $error) {
            $this->error($error->getMessage());

            return self::FAILURE;
        }

        $action = $this->option('dry-run') ? 'Would prune' : 'Pruned';
        $this->info(sprintf(
            '%s %d jobs (%d successful, %d failed), %d bytes.',
            $action,
            $result['jobs'],
            $result['success_jobs'],
            $result['failure_jobs'],
            $result['bytes'],
        ));

        return self::SUCCESS;
    }
}
