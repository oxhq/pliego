<?php

declare(strict_types=1);

namespace Pliego\Laravel;

use Illuminate\Contracts\View\Factory as ViewFactory;
use Illuminate\Http\Client\Factory as HttpFactory;
use Illuminate\Support\ServiceProvider;
use Pliego\Laravel\Console\DoctorCommand;
use Pliego\Laravel\Console\InstallCommand;
use Pliego\Laravel\Console\PruneCommand;
use Pliego\Php\CliRenderer;
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
        $this->app->singleton(CliRenderer::class, function ($app): CliRenderer {
            $runtimeStartedAt = hrtime(true);
            $binary = $app->make(ManagedRuntime::class)->binary();

            return new CliRenderer(
                [$binary],
                (int) $app['config']->get('pliego.timeout_seconds'),
                runtimeResolutionNanoseconds: (int) (hrtime(true) - $runtimeStartedAt),
            );
        });
        $this->app->singleton(DocumentFactory::class, function ($app): DocumentFactory {
            return new DocumentFactory(
                $app->make(ViewFactory::class),
                $app->make(CliRenderer::class),
                (string) $app['config']->get('pliego.work_dir'),
                new RenderOptions(
                    locale: (string) $app['config']->get('pliego.locale'),
                    timezone: (string) $app['config']->get('pliego.timezone'),
                    pageSize: (string) $app['config']->get('pliego.page_size'),
                    pageMargins: (string) $app['config']->get('pliego.page_margins'),
                ),
            );
        });
    }
}
