<?php

declare(strict_types=1);

namespace Pliego\Laravel;

use Illuminate\Contracts\View\Factory as ViewFactory;
use Illuminate\Http\Client\Factory as HttpFactory;
use Illuminate\Support\ServiceProvider;
use Pliego\Laravel\Console\DoctorCommand;
use Pliego\Laravel\Console\InstallCommand;
use Pliego\Laravel\Console\PruneCommand;
use Pliego\Php\DocumentEngine;
use Pliego\Php\RenderOptions;

final class PliegoServiceProvider extends ServiceProvider
{
    public function boot(): void
    {
        if ($this->app->runningInConsole()) {
            $this->commands([InstallCommand::class, DoctorCommand::class, PruneCommand::class]);
        }
    }

    public function register(): void
    {
        $this->mergeConfigFrom(dirname(__DIR__).'/config/pliego.php', 'pliego');

        $this->app->singleton(ManagedRuntime::class, function ($app): ManagedRuntime {
            $override = $app['config']->get('pliego.binary');

            return new ManagedRuntime(
                (string) $app['config']->get('pliego.runtime_dir'),
                dirname(__DIR__).'/resources/runtimes.json',
                $app->make(HttpFactory::class),
                is_string($override) ? $override : null,
            );
        });
        $this->app->singleton(DocumentEngine::class, function ($app): DocumentEngine {
            $runtimeStartedAt = hrtime(true);
            $binary = $app->make(ManagedRuntime::class)->binary();

            return new DocumentEngine(
                [$binary],
                (string) $app['config']->get('pliego.work_dir'),
                timeoutSeconds: (int) $app['config']->get('pliego.timeout_seconds'),
                runtimeResolutionNanoseconds: (int) (hrtime(true) - $runtimeStartedAt),
            );
        });
        $this->app->singleton(DocumentFactory::class, function ($app): DocumentFactory {
            $defaultStorageDisk = $app['config']->get('filesystems.default');

            return new DocumentFactory(
                $app->make(ViewFactory::class),
                $app->make(DocumentEngine::class),
                new RenderOptions(
                    locale: (string) $app['config']->get('pliego.locale'),
                    timezone: (string) $app['config']->get('pliego.timezone'),
                    pageSize: (string) $app['config']->get('pliego.page_size'),
                    pageMargins: (string) $app['config']->get('pliego.page_margins'),
                ),
                $app->make('filesystem'),
                is_string($defaultStorageDisk) ? $defaultStorageDisk : null,
            );
        });
    }
}
