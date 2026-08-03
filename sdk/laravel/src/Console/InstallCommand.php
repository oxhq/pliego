<?php

declare(strict_types=1);

namespace Pliego\Laravel\Experimental\Console;

use Illuminate\Console\Command;
use Pliego\Laravel\Experimental\ManagedRuntime;
use Throwable;

final class InstallCommand extends Command
{
    protected $signature = 'pliego:install';

    protected $description = 'Install the verified native Pliego runtime for this platform';

    public function handle(ManagedRuntime $runtime): int
    {
        try {
            $binary = $runtime->install();
        } catch (Throwable $error) {
            $this->error($error->getMessage());

            return self::FAILURE;
        }

        $this->info("Pliego {$runtime->version()} installed: {$binary}");

        return self::SUCCESS;
    }
}
