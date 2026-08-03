<?php

declare(strict_types=1);

namespace Pliego\Laravel\Experimental\Console;

use Illuminate\Console\Command;
use Pliego\Laravel\Experimental\ManagedRuntime;
use Pliego\Php\Experimental\Doctor;
use Throwable;

final class DoctorCommand extends Command
{
    protected $signature = 'pliego:doctor';

    protected $description = 'Check the Pliego binary, work root, and one offline render';

    public function handle(ManagedRuntime $runtime): int
    {
        try {
            $report = (new Doctor(
                [$runtime->binary()],
                (int) config('pliego.timeout_seconds'),
            ))->run((string) config('pliego.work_dir'));
        } catch (Throwable $error) {
            $this->error($error->getMessage());

            return self::FAILURE;
        }

        $this->info("Pliego {$report['version']}: {$report['binary']}");
        $this->info("API: {$report['api_version']}");
        $this->info("Platform: {$report['platform']}");
        $this->info("Writable work root: {$report['work_root']}");
        $this->info("Offline smoke PDF: {$report['smoke_pdf']}");

        return self::SUCCESS;
    }
}
