<?php

declare(strict_types=1);

namespace Pliego\Laravel\Experimental;

use Illuminate\Contracts\View\Factory as ViewFactory;
use Illuminate\Http\Client\Factory as HttpFactory;
use Illuminate\Support\ServiceProvider;
use Pliego\Laravel\Experimental\Console\DoctorCommand;
use Pliego\Laravel\Experimental\Console\InstallCommand;
use Pliego\Laravel\Experimental\Console\PruneCommand;
use Pliego\Php\Experimental\CliRenderer;
use Pliego\Php\Experimental\RenderOptions;

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
            return new CliRenderer(
                [$app->make(ManagedRuntime::class)->binary()],
                (int) $app['config']->get('pliego.timeout_seconds'),
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
